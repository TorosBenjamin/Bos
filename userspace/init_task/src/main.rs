#![no_std]
#![no_main]

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    ulib::default_panic(info)
}

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point() -> ! {
    // Create IPC channel for display_server
    let (send_ep, recv_ep) = ulib::sys_channel_create(16);

    // Load and spawn display_server
    let ds_size = ulib::sys_get_module("display_server", core::ptr::null_mut(), 0);
    let ds_buf = ulib::sys_mmap(ds_size, kernel_api_types::MMAP_WRITE);
    let written = ulib::sys_get_module("display_server", ds_buf, ds_size);
    let _ = written;

    let ds_elf_bytes = unsafe { core::slice::from_raw_parts(ds_buf, ds_size as usize) };
    let ds_id = ulib::sys_spawn(ds_elf_bytes, recv_ep);
    ulib::sys_munmap(ds_buf, ds_size);

    // Transfer display ownership to display_server
    ulib::sys_transfer_display(ds_id);

    // Give display_server a moment to initialize
    for _ in 0..10 {
        ulib::sys_yield();
    }

    // Spawn bouncing_cube_1 client
    let cube1_size = ulib::sys_get_module("bouncing_cube_1", core::ptr::null_mut(), 0);
    if cube1_size > 0 {
        let cube1_buf = ulib::sys_mmap(cube1_size, kernel_api_types::MMAP_WRITE);
        let written = ulib::sys_get_module("bouncing_cube_1", cube1_buf, cube1_size);
        let _ = written;

        let cube1_elf = unsafe { core::slice::from_raw_parts(cube1_buf, cube1_size as usize) };
        // Pass display_server send endpoint as arg
        let _cube1_id = ulib::sys_spawn(cube1_elf, send_ep);
        ulib::sys_munmap(cube1_buf, cube1_size);
    }

    // 5. Spawn bouncing_cube_2 client
    let cube2_size = ulib::sys_get_module("bouncing_cube_2", core::ptr::null_mut(), 0);
    if cube2_size > 0 {
        let cube2_buf = ulib::sys_mmap(cube2_size, kernel_api_types::MMAP_WRITE);
        let written = ulib::sys_get_module("bouncing_cube_2", cube2_buf, cube2_size);
        let _ = written;

        let cube2_elf = unsafe { core::slice::from_raw_parts(cube2_buf, cube2_size as usize) };
        // Pass display_server send endpoint as arg
        let _cube2_id = ulib::sys_spawn(cube2_elf, send_ep);
        ulib::sys_munmap(cube2_buf, cube2_size);
    }

    // 6. Init task stays alive, yielding forever
    loop {
        ulib::sys_yield();
    }
}
