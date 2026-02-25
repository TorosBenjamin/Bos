use kernel::interrupt::handlers::{TIMER_STACK_ALIGNMENT_OK};
use crate::TestResult;
use kernel::task::task::Task;
use kernel::task::global_scheduler::spawn_task;
use kernel::task::context;
use kernel::time::tsc;
use core::sync::atomic::{AtomicBool, Ordering};
use alloc::format;

static STACK_ALIGNMENT_OK: AtomicBool = AtomicBool::new(false);

fn check_stack_alignment() -> ! {
    let rsp: u64;
    unsafe {
        core::arch::asm!("mov {}, rsp", out(reg) rsp);
    }
    
    // In x86_64, the stack should be 16-byte aligned before a call.
    // Inside a function, it might be 16-byte aligned or 8-byte off (due to return address).
    // However, since we are at the top level of a task (no one called us with 'call', 
    // we jumped here), it depends on how we set up the stack.
    if rsp % 16 == 0 {
        STACK_ALIGNMENT_OK.store(true, Ordering::SeqCst);
    } else {
        log::error!("Stack not 16-byte aligned: {:#x}", rsp);
    }
    
    loop {
        core::hint::spin_loop();
    }
}

pub fn test_stack_alignment() -> TestResult {
    STACK_ALIGNMENT_OK.store(false, Ordering::SeqCst);
    
    spawn_task(Task::new(check_stack_alignment));
    
    let start_tsc = tsc::value();
    let timeout = tsc::TSC_HZ.load(Ordering::SeqCst); // 1s
    
    let interrupts_enabled = x86_64::instructions::interrupts::are_enabled();
    if !interrupts_enabled {
        x86_64::instructions::interrupts::enable();
    }

    while !STACK_ALIGNMENT_OK.load(Ordering::SeqCst) {
        if tsc::value() - start_tsc > timeout {
            return TestResult::Failed(format!("Stack alignment test timed out"));
        }
        x86_64::instructions::hlt();
    }
    
    TestResult::Ok
}

pub fn test_context_switch_registers() -> TestResult {
    let mut rsp_storage: usize = 0;
    let ok: u64;
    
    unsafe {
        core::arch::asm!(
            "push rbx",
            "push rbp",
            "mov r12, 0x1234567812345678",
            "mov r13, 0x8765432187654321",
            "mov r14, 0xABCDEF01ABCDEF01",
            "mov r15, 0x1020304050607080",
            "mov rbx, 0x1122334455667788",
            "mov rbp, 0x99AABBCCDDEEFF00",
            
            "mov rdi, {prev_rsp_ptr}",
            "mov rsi, rsp",
            "sub rsi, 72", // switch pushes fake rip (8) + 8 registers (64) = 72 bytes
            "call {switch_fn}",
            
            "mov {ok_reg}, 1",
            "mov rax, 0x1234567812345678",
            "cmp r12, rax",
            "je 2f",
            "mov {ok_reg}, 0",
            "2:",
            "mov rax, 0x8765432187654321",
            "cmp r13, rax",
            "je 3f",
            "mov {ok_reg}, 0",
            "3:",
            "mov rax, 0xABCDEF01ABCDEF01",
            "cmp r14, rax",
            "je 4f",
            "mov {ok_reg}, 0",
            "4:",
            "mov rax, 0x1020304050607080",
            "cmp r15, rax",
            "je 5f",
            "mov {ok_reg}, 0",
            "5:",
            "mov rax, 0x1122334455667788",
            "cmp rbx, rax",
            "je 6f",
            "mov {ok_reg}, 0",
            "6:",
            "mov rax, 0x99AABBCCDDEEFF00",
            "cmp rbp, rax",
            "je 7f",
            "mov {ok_reg}, 0",
            "7:",
            "pop rbp",
            "pop rbx",
            prev_rsp_ptr = in(reg) &mut rsp_storage,
            switch_fn = sym context::switch,
            ok_reg = out(reg) ok,
            out("r12") _, out("r13") _, out("r14") _, out("r15") _,
            out("rdi") _, out("rsi") _, out("rax") _
        );
    }
    
    if ok == 1 {
        TestResult::Ok
    } else {
        TestResult::Failed(format!("Registers not preserved during context switch"))
    }
}

pub fn test_timer_stack_alignment() -> TestResult {
    TIMER_STACK_ALIGNMENT_OK.store(false, Ordering::SeqCst);
    
    let interrupts_enabled = x86_64::instructions::interrupts::are_enabled();
    if !interrupts_enabled {
        x86_64::instructions::interrupts::enable();
    }
    
    let start_tsc = tsc::value();
    let timeout = tsc::TSC_HZ.load(Ordering::SeqCst); // 1s
    
    while !TIMER_STACK_ALIGNMENT_OK.load(Ordering::SeqCst) {
        if tsc::value() - start_tsc > timeout {
            return TestResult::Failed(format!("Timer stack alignment test timed out"));
        }
        x86_64::instructions::hlt();
    }
    
    TestResult::Ok
}
