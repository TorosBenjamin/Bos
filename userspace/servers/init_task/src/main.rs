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

    // Load and spawn display_server (it will self-register the "display" service)
    let ds_size = ulib::sys_get_module("display_server", core::ptr::null_mut(), 0);
    let ds_buf = ulib::sys_mmap(ds_size, kernel_api_types::MMAP_WRITE);
    let _ = ulib::sys_get_module("display_server", ds_buf, ds_size);

    let ds_elf_bytes = unsafe { core::slice::from_raw_parts(ds_buf, ds_size as usize) };
    let ds_id = ulib::sys_spawn(ds_elf_bytes, 0);
    ulib::sys_munmap(ds_buf, ds_size);

    // Transfer display ownership to display_server
    ulib::sys_transfer_display(ds_id);

    // Load and spawn fs_server (registers "fatfs" service)
    let fss_size = ulib::sys_get_module("fs_server", core::ptr::null_mut(), 0);
    if fss_size > 0 {
        let fss_buf = ulib::sys_mmap(fss_size, kernel_api_types::MMAP_WRITE);
        let _ = ulib::sys_get_module("fs_server", fss_buf, fss_size);
        let fss_elf = unsafe { core::slice::from_raw_parts(fss_buf, fss_size as usize) };
        let _ = ulib::sys_spawn(fss_elf, 0);
        ulib::sys_munmap(fss_buf, fss_size);
    }

    if is_test_mode {
        // Test mode: spawn utest; skip normal apps
        let utest_buf = ulib::sys_mmap(utest_size, kernel_api_types::MMAP_WRITE);
        let _ = ulib::sys_get_module("utest", utest_buf, utest_size);

        let utest_elf = unsafe { core::slice::from_raw_parts(utest_buf, utest_size as usize) };
        let _ = ulib::sys_spawn(utest_elf, 0);
        ulib::sys_munmap(utest_buf, utest_size);
    } else {
        // Normal mode: spawn files and hello_egui
        let files_size = ulib::sys_get_module("files", core::ptr::null_mut(), 0);
        if files_size > 0 {
            let files_buf = ulib::sys_mmap(files_size, kernel_api_types::MMAP_WRITE);
            let _ = ulib::sys_get_module("files", files_buf, files_size);
            let files_elf =
                unsafe { core::slice::from_raw_parts(files_buf, files_size as usize) };
            let _ = ulib::sys_spawn(files_elf, 0);
            ulib::sys_munmap(files_buf, files_size);
        }

        let hello_egui_size = ulib::sys_get_module("hello_egui", core::ptr::null_mut(), 0);
        if hello_egui_size > 0 {
            let hello_egui_buf = ulib::sys_mmap(hello_egui_size, kernel_api_types::MMAP_WRITE);
            let _ = ulib::sys_get_module("hello_egui", hello_egui_buf, hello_egui_size);
            let hello_egui_elf =
                unsafe { core::slice::from_raw_parts(hello_egui_buf, hello_egui_size as usize) };
            let _ = ulib::sys_spawn(hello_egui_elf, 0);
            ulib::sys_munmap(hello_egui_buf, hello_egui_size);
        }
    }

    // Init task stays alive, yielding forever
    loop {
        ulib::sys_yield();
    }
}
