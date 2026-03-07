#![no_std]
#![no_main]

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    ulib::default_panic(info)
}

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point() -> ! {
    // Detect test mode: if a "/utest" Limine module is present, run integration tests.
    let utest_size = ulib::sys_get_module("utest", core::ptr::null_mut(), 0);
    let is_test_mode = utest_size > 0;

    // Load and spawn fs_server (registers "fatfs" service)
    let fss_size = ulib::sys_get_module("fs_server", core::ptr::null_mut(), 0);
    if fss_size > 0 {
        let fss_buf = ulib::sys_mmap(fss_size, kernel_api_types::MMAP_WRITE);
        let _ = ulib::sys_get_module("fs_server", fss_buf, fss_size);
        let fss_elf = unsafe { core::slice::from_raw_parts(fss_buf, fss_size as usize) };
        let _ = ulib::sys_spawn(fss_elf, 0);
        ulib::sys_munmap(fss_buf, fss_size);
    }

    // Load and spawn display_server (it will self-register the "display" service)
    let ds_size = ulib::sys_get_module("display_server", core::ptr::null_mut(), 0);
    let ds_buf = ulib::sys_mmap(ds_size, kernel_api_types::MMAP_WRITE);
    let _ = ulib::sys_get_module("display_server", ds_buf, ds_size);

    let ds_elf_bytes = unsafe { core::slice::from_raw_parts(ds_buf, ds_size as usize) };
    let ds_id = ulib::sys_spawn(ds_elf_bytes, 0);
    ulib::sys_munmap(ds_buf, ds_size);

    // Transfer display ownership to display_server
    ulib::sys_transfer_display(ds_id);

    if is_test_mode {
        // Test mode: spawn utest; skip normal apps
        let utest_buf = ulib::sys_mmap(utest_size, kernel_api_types::MMAP_WRITE);
        let _ = ulib::sys_get_module("utest", utest_buf, utest_size);

        let utest_elf = unsafe { core::slice::from_raw_parts(utest_buf, utest_size as usize) };
        let _ = ulib::sys_spawn(utest_elf, 0);
        ulib::sys_munmap(utest_buf, utest_size);
    } else {
        // Normal mode: load apps from FAT32 filesystem
        let fs_ep = ulib::fs::fs_lookup();

        // Normal mode: spawn launcher first (hidden, toggled by Super+Space), then regular apps.
        for (path, name) in [
            ("LAUNCH.ELF", b"launcher" as &[u8]),
            ("HELLO.ELF",  b"hello_egui"),
            ("HELLO.ELF",  b"hello_egui"),
            ("HELLO.ELF",  b"hello_egui"),
            ("HELLO.ELF",  b"hello_egui"),
            ("HELLO.ELF",  b"hello_egui"),
            ("HELLO.ELF",  b"hello_egui"),
            ("HELLO.ELF",  b"hello_egui"),
        ] {
            if let Some((buf_id, size)) = ulib::fs::fs_map_file(fs_ep, path) {
                let ptr = ulib::sys_map_shared_buf(buf_id);
                if !ptr.is_null() {
                    let elf = unsafe { core::slice::from_raw_parts(ptr as *const u8, size as usize) };
                    let _ = ulib::sys_spawn_named(elf, 0, name);
                    ulib::sys_munmap(ptr, size);
                }
                ulib::sys_destroy_shared_buf(buf_id);
            }
        }
    }

    // Init task stays alive, sleeping to avoid burning CPU
    loop {
        ulib::sys_sleep_ms(10);
    }
}
