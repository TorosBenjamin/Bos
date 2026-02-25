use kernel::interrupt::handlers::TIMER_STACK_ALIGNMENT_OK;
use kernel::task::context::context_switch_regular;
use kernel::task::global_scheduler::spawn_task;
use kernel::task::task::Task;
use kernel::time::tsc;
use crate::TestResult;
use core::sync::atomic::{AtomicBool, Ordering};
use alloc::format;

static STACK_ALIGNMENT_OK: AtomicBool = AtomicBool::new(false);

fn check_stack_alignment() -> ! {
    let rsp: u64;
    unsafe {
        core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nostack, nomem));
    }
    // A kernel task entry point is jumped to (not called), so RSP should be
    // 16-byte aligned at the very first instruction.
    if rsp % 16 == 0 {
        STACK_ALIGNMENT_OK.store(true, Ordering::Release);
    } else {
        log::error!("Task entry RSP not 16-byte aligned: {:#x}", rsp);
    }
    loop {
        core::hint::spin_loop();
    }
}

/// Verify that a newly-spawned kernel task's entry RSP is 16-byte aligned.
pub fn test_stack_alignment() -> TestResult {
    STACK_ALIGNMENT_OK.store(false, Ordering::SeqCst);

    spawn_task(Task::new(check_stack_alignment));

    let start_tsc = tsc::value();
    let timeout = tsc::TSC_HZ.load(Ordering::SeqCst); // ~1 s

    if !x86_64::instructions::interrupts::are_enabled() {
        x86_64::instructions::interrupts::enable();
    }

    while !STACK_ALIGNMENT_OK.load(Ordering::Acquire) {
        if tsc::value() - start_tsc > timeout {
            return TestResult::Failed(format!("Stack alignment test timed out"));
        }
        x86_64::instructions::hlt();
    }

    TestResult::Ok
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

/// Verify that the LAPIC timer interrupt inner handler is called with a
/// properly 8-byte-aligned RSP (x86-64 ABI: RSP % 16 == 8 on function entry).
pub fn test_timer_stack_alignment() -> TestResult {
    TIMER_STACK_ALIGNMENT_OK.store(false, Ordering::SeqCst);

    if !x86_64::instructions::interrupts::are_enabled() {
        x86_64::instructions::interrupts::enable();
    }

    let start_tsc = tsc::value();
    let timeout = tsc::TSC_HZ.load(Ordering::SeqCst); // ~1 s

    while !TIMER_STACK_ALIGNMENT_OK.load(Ordering::Acquire) {
        if tsc::value() - start_tsc > timeout {
            return TestResult::Failed(format!("Timer stack alignment test timed out"));
        }
        x86_64::instructions::hlt();
    }

    TestResult::Ok
}
