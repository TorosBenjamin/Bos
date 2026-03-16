use crate::{hlt_loop};
use crate::memory::cpu_local_data::{get_local, local_apic_id_of, try_get_local, CURRENT_CONTEXT_PTR_OFFSET, IN_SYSCALL_HANDLER_OFFSET};
use crate::memory::guarded_stack::STACK_GUARD_PAGES;
use crate::task::task::{
    CpuContext, CTX_RAX, CTX_RBP, CTX_RBX, CTX_RCX, CTX_RDI, CTX_RDX, CTX_RSI,
    CTX_R8, CTX_R9, CTX_R10, CTX_R11, CTX_R12, CTX_R13, CTX_R14, CTX_R15,
    CTX_RIP, CTX_CS, CTX_RFLAGS, CTX_RSP, CTX_SS,
};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use x86_64::structures::idt::{InterruptStackFrame, PageFaultErrorCode};

pub static TIMER_INTERRUPT_COUNT: AtomicU64 = AtomicU64::new(0);
/// Set to `true` by the timer interrupt inner handler once it fires with a
/// properly 8-byte-aligned stack. Used by `test_timer_stack_alignment`.
pub static TIMER_STACK_ALIGNMENT_OK: AtomicBool = AtomicBool::new(false);
use crate::interrupt::nmi_handler_state::{NmiHandlerState, NMI_HANDLER_STATES};

/// Page fault handler with swapgs and SS RPL fix.
///
/// The demand-paging success path returns to user mode via iretq. Without the
/// SS RPL fix, KVM's stripped SS (0x18 instead of 0x1B) would cause a #GP on
/// iretq. This naked wrapper applies the same fix as the timer/keyboard/mouse
/// handlers.
///
/// CPU pushes error_code before the iretq frame, so we skip it (`add rsp, 8`)
/// before the iretq epilogue.
#[unsafe(naked)]
pub extern "C" fn page_fault_handler() {
    core::arch::naked_asm!(
        // Entry: [rsp+0]=error_code, [rsp+8]=rip, [rsp+16]=cs, ...
        // --- Entry swapgs ---
        "push r11",
        "mov r11, [rsp + 24]",   // CS: +8(r11) +8(err) +8(rip) = +24
        "test r11, 3",
        "jz 1f",
        "swapgs",
        "1:",
        "pop r11",

        // Save 9 caller-saved registers
        "push rax",
        "push rcx",
        "push rdx",
        "push rsi",
        "push rdi",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        // Stack: [9 regs=72][error_code][rip][cs][rflags][rsp][ss]

        // Read CR2 before anything else can overwrite it
        "mov rdi, cr2",          // arg1: faulting address
        "mov rsi, [rsp + 72]",   // arg2: error_code
        "lea rdx, [rsp + 80]",   // arg3: pointer to iretq frame

        "call {inner}",

        // Inner returned → demand paging succeeded, return to user mode.
        // (All other paths in inner are -> ! and never reach here.)

        // Restore 8 of 9 saved registers (rax stays for SS fix)
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rdi",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        // Stack: [rax][error_code][rip][cs][rflags][rsp][ss]

        // Pop rax, skip error_code
        "pop rax",
        "add rsp, 8",
        // Stack: [rip][cs][rflags][rsp][ss]

        // --- SS RPL fix + swapgs epilogue (same as keyboard handler) ---
        "push rax",
        // Stack: [rax][rip][cs][rflags][rsp][ss]
        "mov rax, [rsp + 16]",   // CS
        "and rax, 3",
        "cmp rax, 3",
        "jne 2f",
        "mov rax, [rsp + 40]",   // SS
        "or rax, 3",
        "mov [rsp + 40], rax",
        "2:",
        "mov rax, [rsp + 16]",   // CS
        "test rax, 3",
        "jz 3f",
        "swapgs",
        "3:",
        "pop rax",
        "iretq",

        inner = sym page_fault_handler_inner,
    )
}

/// Inner page fault handler called from the naked wrapper.
///
/// Returns normally only when demand paging succeeds (the wrapper will iretq
/// back to the faulting instruction). All other paths (`kill_from_exception`
/// or `panic!`) never return.
///
/// # Arguments
/// * `cr2` — faulting virtual address (from CR2)
/// * `error_code` — raw page fault error code
/// * `iretq_frame` — pointer to `[rip, cs, rflags, rsp, ss]` on the stack
extern "C" fn page_fault_handler_inner(cr2: u64, error_code: u64, iretq_frame: *const u64) {
    let cs = unsafe { *iretq_frame.add(1) };
    let rip = unsafe { *iretq_frame };

    if cs & 3 == 3 {
        // Ring 3 page fault — swapgs already done by wrapper.
        let pf_code = PageFaultErrorCode::from_bits_truncate(error_code);
        if !pf_code.contains(PageFaultErrorCode::PROTECTION_VIOLATION)
            && crate::memory::demand::try_demand_fill(cr2)
        {
            return; // wrapper does SS fix + swapgs + iretq
        }
        log::warn!(
            "User task page fault: addr={:#x} ip={:#x} err={:?} — killing task",
            cr2, rip, pf_code,
        );
        crate::syscall_handlers::kill_from_exception(
            kernel_api_types::FAULT_PAGE_FAULT,
            cr2,
            rip,
        );
    }

    // Kernel-mode fault — check guard pages then panic.
    let accessed_address = x86_64::VirtAddr::new(cr2);
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
            "Page fault! addr={accessed_address:?} ip={rip:#x} err={error_code:#x}"
        );
    }
}

/// Try to safely read the 5-entry iretq frame at `rsp`.
///
/// Returns `None` (instead of faulting) if:
/// - `rsp` is misaligned
/// - `rsp..rsp+40` overflows or falls outside the HHDM
/// - the page(s) containing the range are not mapped
fn try_read_iretq_frame(rsp: u64) -> Option<[u64; 5]> {
    use x86_64::registers::control::Cr3;
    use x86_64::structures::paging::{OffsetPageTable, PageTable, Translate};
    use x86_64::VirtAddr;

    if !rsp.is_multiple_of(8) {
        return None;
    }
    let end = rsp.checked_add(5 * 8)?;

    let hhdm = crate::limine_requests::HHDM_REQUEST
        .get_response()?
        .offset();
    if rsp < hhdm || end < hhdm {
        return None;
    }

    // Use the current page table to verify the page(s) are actually mapped.
    let (cr3_frame, _) = Cr3::read();
    let l4_virt = VirtAddr::new(hhdm + cr3_frame.start_address().as_u64());
    let l4_table = unsafe { &mut *l4_virt.as_mut_ptr::<PageTable>() };
    let mapper = unsafe { OffsetPageTable::new(l4_table, VirtAddr::new(hhdm)) };

    // Check start page.
    mapper.translate_addr(VirtAddr::new(rsp))?;
    // If the range crosses a page boundary, check the end page too.
    let start_page = rsp & !0xFFF;
    let end_page = (end - 1) & !0xFFF;
    if start_page != end_page && mapper.translate_addr(VirtAddr::new(end_page)).is_none() {
        return None;
    }

    let ptr = rsp as *const u64;
    Some(unsafe { [*ptr, *ptr.add(1), *ptr.add(2), *ptr.add(3), *ptr.add(4)] })
}

pub extern "x86-interrupt" fn general_protection_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    // Ring-3 GPF: unaligned SSE access, bad segment selector, etc. — kill the task.
    if stack_frame.code_segment.0 & 3 == 3 {
        unsafe { core::arch::asm!("swapgs", options(nostack, nomem)); }
        log::warn!(
            "User task GPF: ip={:#x} err={} — killing task",
            stack_frame.instruction_pointer.as_u64(),
            error_code,
        );
        crate::syscall_handlers::kill_from_exception(
            kernel_api_types::FAULT_GPF,
            0,
            stack_frame.instruction_pointer.as_u64(),
        );
    }

    // Kernel-mode GPF: if the fault was during iretq, try to dump the iretq frame.
    // RSP may be corrupted, so validate thoroughly before dereferencing to avoid
    // a recursive #GPF → double fault with an unhelpful message.
    let rsp = stack_frame.stack_pointer.as_u64();
    match try_read_iretq_frame(rsp) {
        Some([rip, cs, rflags, user_rsp, ss]) => {
            log::error!(
                "IRETQ frame at RSP {:#x}: rip={:#x} cs={:#x} rflags={:#x} rsp={:#x} ss={:#x}",
                rsp, rip, cs, rflags, user_rsp, ss
            );
        }
        None => {
            log::error!("RSP {:#x} appears corrupt, skipping iretq frame dump", rsp);
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

pub extern "x86-interrupt" fn divide_error_handler(stack_frame: InterruptStackFrame) {
    // Ring-3 divide by zero: kill the task.
    if stack_frame.code_segment.0 & 3 == 3 {
        unsafe { core::arch::asm!("swapgs", options(nostack, nomem)); }
        log::warn!(
            "User task divide by zero: ip={:#x} — killing task",
            stack_frame.instruction_pointer.as_u64(),
        );
        crate::syscall_handlers::kill_from_exception(
            kernel_api_types::FAULT_DIVIDE_BY_ZERO,
            0,
            stack_frame.instruction_pointer.as_u64(),
        );
    }
    panic!("Kernel divide by zero! Stack frame: {stack_frame:#?}");
}

pub extern "x86-interrupt" fn breakpoint_handler(mut stack_frame: InterruptStackFrame) {
    log::info!("Breakpoint! Stack frame: {stack_frame:#?}");
    if stack_frame.code_segment.0 & 3 == 3 {
        unsafe {
            stack_frame.as_mut().update(|f| {
                if f.stack_segment.0 & 3 == 0 { f.stack_segment.0 |= 3; }
            });
        }
    }
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
        // Save r11 first so we can use it as scratch without losing its value.
        "push r11",
        // After push, stack layout: [r11][rip][cs][rflags][rsp][ss]
        // CS is now at rsp+16 (was rsp+8 before the push).
        "mov r11, [rsp + 16]",  // CS from interrupt frame
        "test r11, 3",
        "jz 9f",                // RPL=0, from kernel, skip swapgs
        "swapgs",
        "9:",

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

    // Same alignment check as timer_interrupt_handler_inner: verify the call
    // site (i.e. the naked-asm timer handler) has a properly 8-byte-aligned RSP.
    // This fires when no task is scheduled (e.g. during timer_interrupt_fires),
    // so the flag gets set well before test_timer_stack_alignment runs.
    if !TIMER_STACK_ALIGNMENT_OK.load(Ordering::Relaxed) {
        let rsp: u64;
        unsafe { core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nostack, nomem)) };
        if rsp.is_multiple_of(8) {
            TIMER_STACK_ALIGNMENT_OK.store(true, Ordering::Release);
        }
    }
}

/// Bootstrap the first task when there's no current task running.
/// Returns the context pointer of the first task, or null if no tasks available.
///
/// EOI is sent only when a task is found and returned. When null is returned,
/// the caller falls through to the early-exit path which calls `timer_early_eoi`
/// — that function is responsible for sending EOI in the no-task case.
extern "C" fn timer_bootstrap_first_task() -> *mut CpuContext {
    let cpu = get_local();

    // Check if run queue is initialized
    if cpu.run_queue.get().is_none() {
        // No scheduler initialized yet — let timer_early_eoi handle EOI
        return core::ptr::null_mut();
    }

    // Try to get a task from the scheduler
    let ctx = crate::task::local_scheduler::schedule_from_interrupt(cpu);
    if ctx.is_null() {
        // No task available — let timer_early_eoi handle EOI
        return core::ptr::null_mut();
    }

    // Found a task: tick accounting and EOI before switching via iretq
    crate::time::on_timer_tick();
    unsafe {
        let local_apic = &mut *cpu.local_apic.get().unwrap().get();
        local_apic.end_of_interrupt()
    };
    TIMER_INTERRUPT_COUNT.fetch_add(1, Ordering::Relaxed);

    ctx
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

    // Verify the interrupt handler was called with an 8-byte-aligned RSP (the
    // x86-64 ABI guarantees RSP % 16 == 8 on function entry due to the return
    // address push). Any misalignment here would indicate a broken interrupt
    // entry path.
    if !TIMER_STACK_ALIGNMENT_OK.load(Ordering::Relaxed) {
        let rsp: u64;
        unsafe { core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nostack, nomem)) };
        if rsp.is_multiple_of(8) {
            TIMER_STACK_ALIGNMENT_OK.store(true, Ordering::Release);
        }
    }

    crate::syscall_handlers::check_timeout_waiters();

    crate::task::local_scheduler::schedule_from_interrupt(cpu)
}

extern "C" fn keyboard_interrupt_inner() {
    crate::drivers::keyboard::on_keyboard_interrupt();
    let cpu = get_local();
    unsafe {
        let local_apic = &mut *cpu.local_apic.get().unwrap().get();
        local_apic.end_of_interrupt();
    }
}

/// Keyboard interrupt handler with swapgs and SS RPL fix.
///
/// Two issues handled:
/// 1. swapgs: with a real KernelGsBase/GsBase split, any interrupt from ring 3
///    must swapgs on entry (GsBase=0 → kernel ptr) and on exit (kernel ptr → 0).
/// 2. SS RPL stripping: KVM sometimes strips RPL bits from the pushed SS value
///    (0x1B → 0x18), causing a #GP on iretq back to ring 3.
#[unsafe(naked)]
pub extern "C" fn keyboard_interrupt_handler() {
    core::arch::naked_asm!(
        // --- Entry swapgs ---
        // Use r11 as scratch to inspect the interrupted CS without touching rax yet.
        // After one push, CS is at [rsp+16].
        "push r11",
        "mov r11, [rsp + 16]",  // CS from iretq frame
        "test r11, 3",
        "jz 4f",                // RPL=0 → kernel mode, skip swapgs
        "swapgs",
        "4:",
        "pop r11",

        // Save all caller-saved registers (9 pushes).
        // On interrupt entry RSP % 16 == 8 (hardware pushed 5 × 8 = 40 bytes).
        // After 9 pushes (72 bytes) RSP % 16 == 0, so `call` leaves RSP % 16 == 8
        // at the callee entry — correct per the SysV64 ABI.
        "push rax",
        "push rcx",
        "push rdx",
        "push rsi",
        "push rdi",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "call {inner}",
        // Restore 8 of the 9 saved registers (rax stays on stack for the SS fix).
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rdi",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        // Stack layout now: [rsp+0]=saved_rax [rsp+8]=rip [rsp+16]=cs
        //                   [rsp+24]=rflags   [rsp+32]=user_rsp [rsp+40]=ss
        // Fix SS RPL if returning to ring 3 (KVM corrupts 0x1B → 0x18).
        "mov rax, [rsp + 16]",
        "and rax, 3",
        "cmp rax, 3",
        "jne 2f",
        "mov rax, [rsp + 40]",
        "or  rax, 3",
        "mov [rsp + 40], rax",
        "2:",
        // --- Exit swapgs ---
        "mov rax, [rsp + 16]",  // CS
        "test rax, 3",
        "jz 5f",                // RPL=0 → kernel mode, skip swapgs
        "swapgs",
        "5:",
        "pop rax",
        "iretq",
        inner = sym keyboard_interrupt_inner,
    )
}

extern "C" fn mouse_interrupt_inner() {
    crate::drivers::mouse::on_mouse_interrupt();
    let cpu = get_local();
    unsafe {
        let local_apic = &mut *cpu.local_apic.get().unwrap().get();
        local_apic.end_of_interrupt();
    }
}

#[unsafe(naked)]
pub extern "C" fn mouse_interrupt_handler() {
    core::arch::naked_asm!(
        "push r11",
        "mov r11, [rsp + 16]",
        "test r11, 3",
        "jz 4f",
        "swapgs",
        "4:",
        "pop r11",
        "push rax",
        "push rcx",
        "push rdx",
        "push rsi",
        "push rdi",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "call {inner}",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rdi",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        "mov rax, [rsp + 16]",
        "and rax, 3",
        "cmp rax, 3",
        "jne 2f",
        "mov rax, [rsp + 40]",
        "or  rax, 3",
        "mov [rsp + 40], rax",
        "2:",
        "mov rax, [rsp + 16]",
        "test rax, 3",
        "jz 5f",
        "swapgs",
        "5:",
        "pop rax",
        "iretq",
        inner = sym mouse_interrupt_inner,
    )
}

extern "C" fn ata_dma_interrupt_inner() {
    crate::drivers::disk::on_ata_interrupt();
    let cpu = get_local();
    unsafe {
        let local_apic = &mut *cpu.local_apic.get().unwrap().get();
        local_apic.end_of_interrupt();
    }
}

/// ATA DMA interrupt handler (IRQ 14) with swapgs and SS RPL fix.
#[unsafe(naked)]
pub extern "C" fn ata_dma_interrupt_handler() {
    core::arch::naked_asm!(
        "push r11",
        "mov r11, [rsp + 16]",
        "test r11, 3",
        "jz 4f",
        "swapgs",
        "4:",
        "pop r11",
        "push rax",
        "push rcx",
        "push rdx",
        "push rsi",
        "push rdi",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "call {inner}",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rdi",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        "mov rax, [rsp + 16]",
        "and rax, 3",
        "cmp rax, 3",
        "jne 2f",
        "mov rax, [rsp + 40]",
        "or  rax, 3",
        "mov [rsp + 40], rax",
        "2:",
        "mov rax, [rsp + 16]",
        "test rax, 3",
        "jz 5f",
        "swapgs",
        "5:",
        "pop rax",
        "iretq",
        inner = sym ata_dma_interrupt_inner,
    )
}

extern "C" fn reschedule_eoi() {
    let cpu = get_local();
    unsafe {
        let local_apic = &mut *cpu.local_apic.get().unwrap().get();
        local_apic.end_of_interrupt();
    }
}

/// Reschedule IPI handler: send EOI then return with swapgs and SS RPL fix.
///
/// Same swapgs + KVM SS-stripping workarounds as `keyboard_interrupt_handler`.
#[unsafe(naked)]
pub extern "C" fn reschedule_ipi_handler() {
    core::arch::naked_asm!(
        // --- Entry swapgs ---
        "push r11",
        "mov r11, [rsp + 16]",  // CS from iretq frame (after 1 push)
        "test r11, 3",
        "jz 4f",
        "swapgs",
        "4:",
        "pop r11",

        "push rax",
        "push rcx",
        "push rdx",
        "push rsi",
        "push rdi",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "call {eoi}",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rdi",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        // Stack layout: [rsp+0]=saved_rax [rsp+8]=rip [rsp+16]=cs
        //               [rsp+24]=rflags   [rsp+32]=user_rsp [rsp+40]=ss
        // Fix SS RPL if returning to ring 3.
        "mov rax, [rsp + 16]",
        "and rax, 3",
        "cmp rax, 3",
        "jne 2f",
        "mov rax, [rsp + 40]",
        "or  rax, 3",
        "mov [rsp + 40], rax",
        "2:",
        // --- Exit swapgs ---
        "mov rax, [rsp + 16]",  // CS
        "test rax, 3",
        "jz 5f",
        "swapgs",
        "5:",
        "pop rax",
        "iretq",
        eoi = sym reschedule_eoi,
    )
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
