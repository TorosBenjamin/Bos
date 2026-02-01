use crate::{hlt_loop};
use crate::memory::cpu_local_data::{get_local, local_apic_id_of, try_get_local};
use crate::memory::guarded_stack::STACK_GUARD_PAGES;
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

#[unsafe(naked)]
pub extern "C" fn timer_interrupt_handler() {
    core::arch::naked_asm!(
        "push rax",
        "push rcx",
        "push rdx",
        "push rbx",
        "push rbp",
        "push rsi",
        "push rdi",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "mov rdi, rsp",
        "call {inner}",
        "mov rsp, rax",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rdi",
        "pop rsi",
        "pop rbp",
        "pop rbx",
        "pop rdx",
        "pop rcx",
        "pop rax",
        "iretq",
        inner = sym timer_interrupt_handler_inner,
    );
}

extern "C" fn timer_interrupt_handler_inner(current_rsp: usize) -> usize {
    let cpu = get_local();

    crate::time::on_timer_tick();

    // Send eoi
    unsafe {
        let local_apic = &mut *cpu.local_apic.get().unwrap().get();
        local_apic.end_of_interrupt()
    };

    TIMER_INTERRUPT_COUNT.fetch_add(1, Ordering::Relaxed);
    let next_rsp = crate::task::local_scheduler::schedule_from_interrupt(cpu, current_rsp);

    // When returning to ring 3, SS must have RPL=3.
    // The CPU clears SS to a NULL-like selector on privilege-level-changing
    // interrupts in 64-bit mode; the pushed value may lack RPL bits.
    {
        let iretq_base = (next_rsp + 15 * 8) as *const u64;
        let cs = unsafe { *iretq_base.add(1) };
        if cs & 3 == 3 {
            let ss_ptr = unsafe { iretq_base.add(4) } as *mut u64;
            let ss = unsafe { *ss_ptr };
            if ss & 3 != 3 {
                unsafe { *ss_ptr = ss | 3 };
            }
        }
    }

    next_rsp
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
