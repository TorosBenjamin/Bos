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

    if is_test_mode {
        // Test mode: spawn utest; skip bouncing cubes
        let utest_buf = ulib::sys_mmap(utest_size, kernel_api_types::MMAP_WRITE);
        let _ = ulib::sys_get_module("utest", utest_buf, utest_size);

        let utest_elf = unsafe { core::slice::from_raw_parts(utest_buf, utest_size as usize) };
        let _ = ulib::sys_spawn(utest_elf, 0);
        ulib::sys_munmap(utest_buf, utest_size);
    } else {
        // Normal mode: spawn bouncing cube clients
        let cube1_size = ulib::sys_get_module("bouncing_cube_1", core::ptr::null_mut(), 0);
        if cube1_size > 0 {
            let cube1_buf = ulib::sys_mmap(cube1_size, kernel_api_types::MMAP_WRITE);
            let _ = ulib::sys_get_module("bouncing_cube_1", cube1_buf, cube1_size);

            let cube1_elf =
                unsafe { core::slice::from_raw_parts(cube1_buf, cube1_size as usize) };
            let _ = ulib::sys_spawn(cube1_elf, 0);
            ulib::sys_munmap(cube1_buf, cube1_size);
        }

        let cube2_size = ulib::sys_get_module("bouncing_cube_2", core::ptr::null_mut(), 0);
        if cube2_size > 0 {
            let cube2_buf = ulib::sys_mmap(cube2_size, kernel_api_types::MMAP_WRITE);
            let _ = ulib::sys_get_module("bouncing_cube_2", cube2_buf, cube2_size);

            let cube2_elf =
                unsafe { core::slice::from_raw_parts(cube2_buf, cube2_size as usize) };
            let _ = ulib::sys_spawn(cube2_elf, 0);
            ulib::sys_munmap(cube2_buf, cube2_size);
        }
    }

    // Init task stays alive, yielding forever
    loop {
        ulib::sys_yield();
    }
}
