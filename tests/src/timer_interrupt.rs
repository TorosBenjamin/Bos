use kernel::interrupt::handlers::TIMER_INTERRUPT_COUNT;
use kernel::time::lapic_timer;
use kernel::time::tsc;
use core::sync::atomic::Ordering;
use crate::TestResult;

pub fn timer_interrupt_fires() -> TestResult {
    // 1. Get initial count
    let initial_count = TIMER_INTERRUPT_COUNT.load(Ordering::SeqCst);

    // 2. Enable interrupts if not already enabled
    let interrupts_enabled = x86_64::instructions::interrupts::are_enabled();
    if !interrupts_enabled {
        x86_64::instructions::interrupts::enable();
    }

    // 3. Set a timer deadline (e.g., 10ms = 10,000,000 ns)
    lapic_timer::set_deadline(10_000_000);

    // 4. Wait for the interrupt to fire (with a timeout)
    let start_tsc = tsc::value();
    let tsc_hz = tsc::TSC_HZ.load(Ordering::SeqCst);
    
    // Timeout after ~1000ms (1 second)
    let timeout_ticks = tsc_hz * 1000; 

    while TIMER_INTERRUPT_COUNT.load(Ordering::SeqCst) <= initial_count {
        if tsc::value() - start_tsc > timeout_ticks {
            return TestResult::Failed(alloc::format!(
                "Timer interrupt did not fire within timeout. Initial count: {}, Current count: {}",
                initial_count,
                TIMER_INTERRUPT_COUNT.load(Ordering::SeqCst)
            ));
        }
        x86_64::instructions::hlt();
    }

    TestResult::Ok
}
