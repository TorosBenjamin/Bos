#![no_std]
#![no_main]

// This is a placeholder - it conflicts with the real init_task
// TODO: Remove this file or rename it once we finalize the architecture

#[panic_handler]
fn rust_panic(_info: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point() -> ! {
    loop {
        core::hint::spin_loop();
    }
}
