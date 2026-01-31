#![no_std]
#![no_main]

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    ulib::default_panic(info)
}

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point() -> ! {
    // 1. Query display_server module size
    let ds_size = ulib::sys_get_module("display_server", core::ptr::null_mut(), 0);

    // 2. Allocate buffer and copy module bytes
    let buf = ulib::sys_mmap(ds_size, kernel_api_types::MMAP_WRITE);
    let written = ulib::sys_get_module("display_server", buf, ds_size);
    let _ = written;

    // 3. Create IPC channel (for future use)
    let (_send_ep, recv_ep) = ulib::sys_channel_create(16);

    // 4. Spawn display server
    let elf_bytes = unsafe { core::slice::from_raw_parts(buf, ds_size as usize) };
    let ds_id = ulib::sys_spawn(elf_bytes, recv_ep);

    // 5. Free the buffer
    ulib::sys_munmap(buf, ds_size);

    // 6. Transfer display ownership to display server
    ulib::sys_transfer_display(ds_id);

    // 7. Init stays alive — future: spawn client tasks here
    loop {
        ulib::sys_yield();
    }
}
