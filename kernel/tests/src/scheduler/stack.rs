use kernel::interrupt::handlers::TIMER_STACK_ALIGNMENT_OK;
use kernel::task::task::Task;
use kernel_api_types::Priority;
use crate::TestResult;
use core::sync::atomic::Ordering;
use alloc::format;

/// Verify that a newly-created kernel task's initial RSP is 16-byte aligned.
///
/// The task enters via `iretq` with RSP = CpuContext.rsp = kernel_stack_top.
/// Per the x86-64 ABI, this must be 16-byte aligned.
///
/// This replaces the previous spawn-and-wait approach, which abandoned the test
/// harness once the timer bootstrapped into the spawned task.
pub fn test_stack_alignment() -> TestResult {
    let task = Task::new(|| loop { core::hint::spin_loop() }, 0, Priority::Normal, None);
    let stack_top = task.inner.lock().kernel_stack_top;
    if stack_top.is_multiple_of(16) {
        TestResult::Ok
    } else {
        TestResult::Failed(format!(
            "Kernel task initial RSP {:#x} is not 16-byte aligned",
            stack_top
        ))
    }
}

/// Verify that the LAPIC timer interrupt handler is called with a
/// properly 8-byte-aligned RSP (x86-64 ABI: RSP % 16 == 8 on function entry).
///
/// `TIMER_STACK_ALIGNMENT_OK` is set by `timer_early_eoi` (no-task bootstrap
/// path) or `timer_interrupt_handler_inner` (normal path). This test is
/// self-contained: if the flag is not already set it arms the timer, enables
/// interrupts, and waits up to 100 ms for one tick to arrive.
pub fn test_timer_stack_alignment() -> TestResult {
    if TIMER_STACK_ALIGNMENT_OK.load(Ordering::Acquire) {
        return TestResult::Ok;
    }

    // Arm a one-shot 10 ms timer tick and enable interrupts so the handler runs.
    // The run queue is empty at this point in the Scheduler test group, so the
    // timer goes through timer_bootstrap_first_task → null → timer_early_eoi,
    // which sends EOI and sets TIMER_STACK_ALIGNMENT_OK.
    let was_enabled = x86_64::instructions::interrupts::are_enabled();
    kernel::time::lapic_timer::set_deadline(10_000_000); // 10 ms in nanoseconds
    if !was_enabled {
        x86_64::instructions::interrupts::enable();
    }

    // Wait up to 100 ms for the flag to be set by the interrupt handler.
    let start = kernel::time::tsc::value();
    let tsc_hz = kernel::time::tsc::TSC_TICKS_PER_MS.load(Ordering::SeqCst);
    let timeout = tsc_hz / 10; // 100 ms
    while !TIMER_STACK_ALIGNMENT_OK.load(Ordering::Acquire) {
        if kernel::time::tsc::value() - start > timeout {
            break;
        }
        core::hint::spin_loop();
    }

    if !was_enabled {
        x86_64::instructions::interrupts::disable();
    }

    if TIMER_STACK_ALIGNMENT_OK.load(Ordering::Acquire) {
        TestResult::Ok
    } else {
        TestResult::Failed(format!("Timer interrupt RSP was not 8-byte aligned (TIMER_STACK_ALIGNMENT_OK never set)"))
    }
}
