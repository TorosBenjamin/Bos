use kernel::interrupt::handlers::TIMER_STACK_ALIGNMENT_OK;
use kernel::task::context::context_switch_regular;
use kernel::task::task::Task;
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
    let task = Task::new(|| loop {});
    let stack_top = task.inner.lock().kernel_stack_top;
    if stack_top % 16 == 0 {
        TestResult::Ok
    } else {
        TestResult::Failed(format!(
            "Kernel task initial RSP {:#x} is not 16-byte aligned",
            stack_top
        ))
    }
}

/// Verify that `context_switch_regular` preserves callee-saved registers
/// (r12–r15, rbx, rbp) across a self-switch.
///
/// A "self-switch" works by pointing both prev and next at the same saved
/// frame: the function saves the current registers, then immediately restores
/// them from the same location, and `ret` returns to the instruction after
/// the `call`.
///
/// Byte offset derivation:
///   - `call` pushes 8-byte return address
///   - `save_registers_regular!()` pushes 7 registers × 8 bytes = 56 bytes
///   - Total: 64 bytes below the RSP at the call site
pub fn test_context_switch_registers() -> TestResult {
    let mut rsp_storage: usize = 0;
    let ok: u64;

    unsafe {
        core::arch::asm!(
            // Preserve Rust's rbx/rbp.  LLVM excludes both from inline-asm
            // register allocation, so {prev_rsp_ptr} will never land in either;
            // the push/pop sequence keeps their values intact from Rust's view.
            "push rbx",
            "push rbp",

            // Consume {prev_rsp_ptr} into rdi NOW, before rbx/rbp are
            // overwritten with sentinel values.
            "mov rdi, {prev_rsp_ptr}",

            // Load sentinel values into all callee-saved registers.
            "mov r12, 0x1234567812345678",
            "mov r13, 0x8765432187654321",
            "mov r14, 0xABCDEF01ABCDEF01",
            "mov r15, 0x1020304050607080",
            "mov rbx, 0x1122334455667788",
            "mov rbp, 0x99AABBCCDDEEFF00",

            // rsi = RSP − 64: the saved frame that context_switch_regular will
            // create (8 bytes return address + 7 × 8 byte registers = 64).
            "mov rsi, rsp",
            "sub rsi, 64",
            "call {switch_fn}",

            // After the self-switch all callee-saved registers must be
            // restored to their sentinel values.  Use rcx as the flag so it
            // never aliases rbx/rbp.
            "mov rcx, 1",
            "mov rax, 0x1234567812345678",
            "cmp r12, rax",
            "je 2f",
            "mov rcx, 0",
            "2:",
            "mov rax, 0x8765432187654321",
            "cmp r13, rax",
            "je 3f",
            "mov rcx, 0",
            "3:",
            "mov rax, 0xABCDEF01ABCDEF01",
            "cmp r14, rax",
            "je 4f",
            "mov rcx, 0",
            "4:",
            "mov rax, 0x1020304050607080",
            "cmp r15, rax",
            "je 5f",
            "mov rcx, 0",
            "5:",
            "mov rax, 0x1122334455667788",
            "cmp rbx, rax",
            "je 6f",
            "mov rcx, 0",
            "6:",
            "mov rax, 0x99AABBCCDDEEFF00",
            "cmp rbp, rax",
            "je 7f",
            "mov rcx, 0",
            "7:",

            // Restore Rust's original rbx/rbp.
            "pop rbp",
            "pop rbx",

            prev_rsp_ptr = in(reg) &mut rsp_storage,
            switch_fn    = sym context_switch_regular,
            out("rcx") ok,
            out("r12") _, out("r13") _, out("r14") _, out("r15") _,
            out("rdi") _, out("rsi") _, out("rax") _,
        );
    }

    if ok == 1 {
        TestResult::Ok
    } else {
        TestResult::Failed(format!("Registers not preserved during context switch"))
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
    let tsc_hz = kernel::time::tsc::TSC_HZ.load(Ordering::SeqCst);
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
        TestResult::Failed(format!(
            "Timer interrupt RSP was not 8-byte aligned (TIMER_STACK_ALIGNMENT_OK never set)"
        ))
    }
}
