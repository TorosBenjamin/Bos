use crate::{hlt_loop};
use crate::memory::cpu_local_data::{get_local, local_apic_id_of, try_get_local, CURRENT_CONTEXT_PTR_OFFSET, IN_SYSCALL_HANDLER_OFFSET};
use crate::memory::guarded_stack::STACK_GUARD_PAGES;
use crate::task::task::{
    CpuContext, CTX_RAX, CTX_RBP, CTX_RBX, CTX_RCX, CTX_RDI, CTX_RDX, CTX_RSI,
    CTX_R8, CTX_R9, CTX_R10, CTX_R11, CTX_R12, CTX_R13, CTX_R14, CTX_R15,
    CTX_RIP, CTX_CS, CTX_RFLAGS, CTX_RSP, CTX_SS,
};
use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::registers::control::Cr2;
use x86_64::structures::idt::{InterruptStackFrame, PageFaultErrorCode};

pub static TIMER_INTERRUPT_COUNT: AtomicU64 = AtomicU64::new(0);
use crate::interrupt::nmi_handler_state::{NmiHandlerState, NMI_HANDLER_STATES};

pub extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    let accessed_address = Cr2::read_raw();
    log::error!(
        "Page fault at {:#x}, error: {error_code:#?}, ip: {:#x}",
        accessed_address,
        stack_frame.instruction_pointer.as_u64()
    );
    let accessed_address = x86_64::VirtAddr::new(accessed_address);
    if let Some(stack) = STACK_GUARD_PAGES
        .lock()
        .iter()
        .find_map(|(page, stack_id)| {
            if accessed_address.align_down(page.size()) == page.start_address() {
                Some(*stack_id)
            } else {
                None
            }
        })
    {
        panic!("Stack overflow: {stack:#X?}");
    } else {
        panic!(
            "Page fault! Stack frame: {stack_frame:#?}. Error code: {error_code:#?}. Accessed address: {accessed_address:?}."
        );
    }
}

pub extern "x86-interrupt" fn general_protection_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    // If GPF was during iretq, dump what the iretq frame was
    // The faulting RSP should point to the iretq frame
    let rsp = stack_frame.stack_pointer.as_u64();
    if rsp > 0xFFFF800000000000 {  // Kernel stack
        unsafe {
            let ptr = rsp as *const u64;
            let iretq_rip = *ptr;
            let iretq_cs = *ptr.add(1);
            let iretq_rflags = *ptr.add(2);
            let iretq_rsp = *ptr.add(3);
            let iretq_ss = *ptr.add(4);
            log::error!(
                "IRETQ frame at RSP {:#x}: rip={:#x} cs={:#x} rflags={:#x} rsp={:#x} ss={:#x}",
                rsp, iretq_rip, iretq_cs, iretq_rflags, iretq_rsp, iretq_ss
            );
        }
    }
    panic!("General Protection Fault! Stack frame: {stack_frame:#?}. Error code: {error_code}.")
}

pub extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) -> ! {
    panic!("Double Fault! Stack frame: {stack_frame:#?}. Error code: {error_code}.")
}

pub extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    log::info!("Breakpoint! Stack frame: {stack_frame:#?}");
}

pub extern "x86-interrupt" fn nmi_handler(_stack_frame: InterruptStackFrame) {
    handle_panic_from_other_cpu()
}

/// Timer interrupt handler that saves/restores context from CpuContext structs.
///
/// Flow:
/// 1. Load current_context_ptr from GS-relative per-CPU data
/// 2. If null (no current task), try to bootstrap first task
/// 3. Save all GPRs to the current context struct
/// 4. Copy the iretq frame from the stack to the context struct
/// 5. Call inner handler (which schedules and returns next context ptr)
/// 6. Store new context ptr to GS
/// 7. Copy iretq frame from new context to stack
/// 8. Fix SS RPL if returning to ring 3 (KVM quirk)
/// 9. Restore all GPRs from new context
/// 10. iretq
#[unsafe(naked)]
pub extern "C" fn timer_interrupt_handler() {
    core::arch::naked_asm!(
        // Check if interrupted from ring 3 (CS RPL bits), swapgs if so.
        // At entry rsp points to the hardware-pushed iretq frame: [rip][cs][rflags][rsp][ss]
        "mov r11, [rsp + 8]",   // CS from interrupt frame
        "test r11, 3",
        "jz 9f",                // RPL=0, from kernel, skip swapgs
        "swapgs",
        "9:",

        // Use r11 as scratch to hold current context pointer
        // (r11 is caller-saved so we can clobber it)
        "push r11",

        // Load current_context_ptr from GS:[offset]
        "mov r11, gs:[{ctx_ptr_offset}]",

        // If context ptr is null, we're not in a scheduled task yet
        // Try to bootstrap the first task
        "test r11, r11",
        "jz 2f",

        // Check if in syscall handler (user state already saved in CpuContext)
        "cmp byte ptr gs:[{in_syscall_offset}], 0",
        "jne 6f",

        // Save all GPRs to context struct (r11 holds context ptr)
        // IMPORTANT: Save rax FIRST before using it as scratch
        "mov [r11 + {CTX_RAX}], rax",
        "mov [r11 + {CTX_R15}], r15",
        "mov [r11 + {CTX_R14}], r14",
        "mov [r11 + {CTX_R13}], r13",
        "mov [r11 + {CTX_R12}], r12",
        // r11 was pushed, get original value from stack (now safe to clobber rax)
        "mov rax, [rsp]",
        "mov [r11 + {CTX_R11}], rax",
        "mov [r11 + {CTX_R10}], r10",
        "mov [r11 + {CTX_R9}], r9",
        "mov [r11 + {CTX_R8}], r8",
        "mov [r11 + {CTX_RDI}], rdi",
        "mov [r11 + {CTX_RSI}], rsi",
        "mov [r11 + {CTX_RBP}], rbp",
        "mov [r11 + {CTX_RBX}], rbx",
        "mov [r11 + {CTX_RDX}], rdx",
        "mov [r11 + {CTX_RCX}], rcx",

        // Copy iretq frame from stack to context
        // Stack layout after "push r11": [r11_saved][rip][cs][rflags][rsp][ss]
        // iretq frame starts at rsp+8
        "mov rax, [rsp + 8]",   // rip
        "mov [r11 + {CTX_RIP}], rax",
        "mov rax, [rsp + 16]",  // cs
        "mov [r11 + {CTX_CS}], rax",
        "mov rax, [rsp + 24]",  // rflags
        "mov [r11 + {CTX_RFLAGS}], rax",
        "mov rax, [rsp + 32]",  // rsp
        "mov [r11 + {CTX_RSP}], rax",
        "mov rax, [rsp + 40]",  // ss
        "mov [r11 + {CTX_SS}], rax",

        // Call inner handler - returns pointer to next task's context
        // (rax will be set to return value)
        "call {inner}",

        // Jump to common restore path
        "jmp 5f",

        // Syscall-yield path: user state already saved in CpuContext, skip re-saving
        "6:",
        "mov byte ptr gs:[{in_syscall_offset}], 0",
        "call {inner}",
        "jmp 5f",

        // Bootstrap path: no current task yet
        "2:",
        // Try to get the first task from the scheduler
        "call {bootstrap}",

        // rax = context ptr (or null if no tasks)
        "test rax, rax",
        "jz 4f",  // No tasks, go to early exit

        // Fall through to common restore path
        "5:",
        // rax now contains next context pointer
        // Store it to GS for next time
        "mov gs:[{ctx_ptr_offset}], rax",

        // r11 = next context pointer
        "mov r11, rax",

        // Copy iretq frame from new context to stack
        "mov rax, [r11 + {CTX_RIP}]",
        "mov [rsp + 8], rax",
        "mov rax, [r11 + {CTX_CS}]",
        "mov [rsp + 16], rax",
        "mov rax, [r11 + {CTX_RFLAGS}]",
        "mov [rsp + 24], rax",
        "mov rax, [r11 + {CTX_RSP}]",
        "mov [rsp + 32], rax",
        "mov rax, [r11 + {CTX_SS}]",
        "mov [rsp + 40], rax",

        // Fix SS RPL if returning to ring 3 (KVM quirk)
        // Check if CS has RPL=3
        "mov rax, [rsp + 16]",  // cs
        "and rax, 3",
        "cmp rax, 3",
        "jne 3f",
        // CS is ring 3, ensure SS has RPL=3
        "mov rax, [rsp + 40]",  // ss
        "or rax, 3",
        "mov [rsp + 40], rax",
        "3:",
        // Check if returning to ring 3, swapgs if so.
        // Stack: [rsp+0]=saved_r11, [rsp+8]=rip, [rsp+16]=cs
        "mov rax, [rsp + 16]",  // target CS
        "test rax, 3",
        "jz 8f",
        "swapgs",
        "8:",

        // Restore all GPRs from new context
        "mov r15, [r11 + {CTX_R15}]",
        "mov r14, [r11 + {CTX_R14}]",
        "mov r13, [r11 + {CTX_R13}]",
        "mov r12, [r11 + {CTX_R12}]",
        "mov r10, [r11 + {CTX_R10}]",
        "mov r9, [r11 + {CTX_R9}]",
        "mov r8, [r11 + {CTX_R8}]",
        "mov rdi, [r11 + {CTX_RDI}]",
        "mov rsi, [r11 + {CTX_RSI}]",
        "mov rbp, [r11 + {CTX_RBP}]",
        "mov rbx, [r11 + {CTX_RBX}]",
        "mov rdx, [r11 + {CTX_RDX}]",
        "mov rcx, [r11 + {CTX_RCX}]",
        "mov rax, [r11 + {CTX_RAX}]",
        // Restore r11 last since we're using it as pointer
        "mov r11, [r11 + {CTX_R11}]",

        // Pop the scratch r11 we pushed at entry (now clobbered, that's fine)
        "add rsp, 8",
        "iretq",

        // Early exit path when no task is scheduled and none available
        "4:",
        "pop r11",
        // Still need to send EOI and return
        "call {early_eoi}",
        // Check if returning to ring 3, swapgs if so.
        "mov rax, [rsp + 8]",   // CS from iretq frame (rip at [rsp], cs at [rsp+8])
        "test rax, 3",
        "jz 7f",
        "swapgs",
        "7:",
        "iretq",

        inner = sym timer_interrupt_handler_inner,
        bootstrap = sym timer_bootstrap_first_task,
        early_eoi = sym timer_early_eoi,
        ctx_ptr_offset = const CURRENT_CONTEXT_PTR_OFFSET,
        in_syscall_offset = const IN_SYSCALL_HANDLER_OFFSET,
        CTX_R15 = const CTX_R15,
        CTX_R14 = const CTX_R14,
        CTX_R13 = const CTX_R13,
        CTX_R12 = const CTX_R12,
        CTX_R11 = const CTX_R11,
        CTX_R10 = const CTX_R10,
        CTX_R9 = const CTX_R9,
        CTX_R8 = const CTX_R8,
        CTX_RDI = const CTX_RDI,
        CTX_RSI = const CTX_RSI,
        CTX_RBP = const CTX_RBP,
        CTX_RBX = const CTX_RBX,
        CTX_RDX = const CTX_RDX,
        CTX_RCX = const CTX_RCX,
        CTX_RAX = const CTX_RAX,
        CTX_RIP = const CTX_RIP,
        CTX_CS = const CTX_CS,
        CTX_RFLAGS = const CTX_RFLAGS,
        CTX_RSP = const CTX_RSP,
        CTX_SS = const CTX_SS,
    );
}

/// Called when timer fires but no task is scheduled yet - just send EOI
extern "C" fn timer_early_eoi() {
    let cpu = get_local();
    crate::time::on_timer_tick();
    unsafe {
        let local_apic = &mut *cpu.local_apic.get().unwrap().get();
        local_apic.end_of_interrupt()
    };
    TIMER_INTERRUPT_COUNT.fetch_add(1, Ordering::Relaxed);
}

/// Bootstrap the first task when there's no current task running.
/// Returns the context pointer of the first task, or null if no tasks available.
extern "C" fn timer_bootstrap_first_task() -> *mut CpuContext {
    let cpu = get_local();

    crate::time::on_timer_tick();

    // Send EOI
    unsafe {
        let local_apic = &mut *cpu.local_apic.get().unwrap().get();
        local_apic.end_of_interrupt()
    };

    TIMER_INTERRUPT_COUNT.fetch_add(1, Ordering::Relaxed);

    // Check if run queue is initialized
    if cpu.run_queue.get().is_none() {
        // No scheduler initialized yet, return null
        log::info!("BOOTSTRAP: run queue not initialized");
        return core::ptr::null_mut();
    }

    // Try to get a task from the scheduler
    crate::task::local_scheduler::schedule_from_interrupt(cpu)
}

/// Load a CpuContext and enter it via iretq. Used to launch the first task.
///
/// # Safety
/// - `context` must point to a valid CpuContext
/// - This function never returns
#[unsafe(naked)]
pub unsafe extern "C" fn load_context_and_iretq(context: *const CpuContext) -> ! {
    core::arch::naked_asm!(
        // rdi = context pointer
        "mov r11, rdi",

        // Allocate space for iretq frame on stack
        "sub rsp, 40",

        // Copy iretq frame from context to stack
        "mov rax, [r11 + {CTX_RIP}]",
        "mov [rsp], rax",
        "mov rax, [r11 + {CTX_CS}]",
        "mov [rsp + 8], rax",
        "mov rax, [r11 + {CTX_RFLAGS}]",
        "mov [rsp + 16], rax",
        "mov rax, [r11 + {CTX_RSP}]",
        "mov [rsp + 24], rax",
        "mov rax, [r11 + {CTX_SS}]",

        // Fix SS RPL if returning to ring 3
        "mov rcx, [rsp + 8]",  // cs
        "and rcx, 3",
        "cmp rcx, 3",
        "jne 4f",
        "or rax, 3",
        "4:",
        "mov [rsp + 32], rax",

        // Swap GS if returning to ring 3
        "mov rcx, [rsp + 8]",  // cs
        "test rcx, 3",
        "jz 5f",
        "swapgs",
        "5:",

        // Restore all GPRs from context
        "mov r15, [r11 + {CTX_R15}]",
        "mov r14, [r11 + {CTX_R14}]",
        "mov r13, [r11 + {CTX_R13}]",
        "mov r12, [r11 + {CTX_R12}]",
        "mov r10, [r11 + {CTX_R10}]",
        "mov r9, [r11 + {CTX_R9}]",
        "mov r8, [r11 + {CTX_R8}]",
        "mov rdi, [r11 + {CTX_RDI}]",
        "mov rsi, [r11 + {CTX_RSI}]",
        "mov rbp, [r11 + {CTX_RBP}]",
        "mov rbx, [r11 + {CTX_RBX}]",
        "mov rdx, [r11 + {CTX_RDX}]",
        "mov rcx, [r11 + {CTX_RCX}]",
        "mov rax, [r11 + {CTX_RAX}]",
        // Restore r11 last
        "mov r11, [r11 + {CTX_R11}]",

        "iretq",

        CTX_R15 = const CTX_R15,
        CTX_R14 = const CTX_R14,
        CTX_R13 = const CTX_R13,
        CTX_R12 = const CTX_R12,
        CTX_R11 = const CTX_R11,
        CTX_R10 = const CTX_R10,
        CTX_R9 = const CTX_R9,
        CTX_R8 = const CTX_R8,
        CTX_RDI = const CTX_RDI,
        CTX_RSI = const CTX_RSI,
        CTX_RBP = const CTX_RBP,
        CTX_RBX = const CTX_RBX,
        CTX_RDX = const CTX_RDX,
        CTX_RCX = const CTX_RCX,
        CTX_RAX = const CTX_RAX,
        CTX_RIP = const CTX_RIP,
        CTX_CS = const CTX_CS,
        CTX_RFLAGS = const CTX_RFLAGS,
        CTX_RSP = const CTX_RSP,
        CTX_SS = const CTX_SS,
    )
}

/// Inner handler called by timer interrupt - context is already saved to current task.
/// Returns pointer to next task's CpuContext.
extern "C" fn timer_interrupt_handler_inner() -> *mut CpuContext {
    let cpu = get_local();

    crate::time::on_timer_tick();

    // Send EOI
    unsafe {
        let local_apic = &mut *cpu.local_apic.get().unwrap().get();
        local_apic.end_of_interrupt()
    };

    TIMER_INTERRUPT_COUNT.fetch_add(1, Ordering::Relaxed);

    crate::task::local_scheduler::schedule_from_interrupt(cpu)
}

pub extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    crate::drivers::keyboard::on_keyboard_interrupt();

    // Send EOI to local APIC
    let cpu = get_local();
    unsafe {
        let local_apic = &mut *cpu.local_apic.get().unwrap().get();
        local_apic.end_of_interrupt();
    }
}

// -- NMI ---
pub fn handle_panic_from_other_cpu() -> ! {
    if let Some(local) = try_get_local()
        && let Some(nmi_handler_states) = NMI_HANDLER_STATES.get()
    {
        let local_apic = unsafe {
            &mut *local
                .local_apic
                .get()
                .expect("local APIC not initialized")
                .get()
        };

        for (cpu_id, nmi_handler_state) in nmi_handler_states
            .iter()
            .enumerate()
            .filter(|(cpu_id, _)| *cpu_id as u32 != local.kernel_id)
        {
            if nmi_handler_state.swap(
                NmiHandlerState::KernelPanicked,
                Ordering::Release,
            ) == NmiHandlerState::NmiHandlerSet
            {
                unsafe {
                    local_apic.send_nmi(local_apic_id_of(cpu_id as u32));
                }
            }
        }
    }

    hlt_loop()
}
