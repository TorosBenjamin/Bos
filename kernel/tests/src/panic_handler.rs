use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};
use crate::test_panic_handler;

// This static flag will be set if a panic occurs
pub static PANIC_OCCURRED: AtomicBool = AtomicBool::new(false);

/// This doesn't really work and catching panics is a bit difficult right now
pub fn with_panic_catch<F: FnOnce()>(f: F) -> bool {
    PANIC_OCCURRED.store(false, Ordering::SeqCst);
    f();
    PANIC_OCCURRED.load(Ordering::SeqCst)
}

#[panic_handler]
pub fn panic(_info: &PanicInfo) -> ! {
    PANIC_OCCURRED.store(true, Ordering::SeqCst);
    test_panic_handler(_info);
}

