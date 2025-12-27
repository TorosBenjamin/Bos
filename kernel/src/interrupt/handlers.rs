use crate::{hlt_loop};
use crate::memory::cpu_local_data::{get_local, local_apic_id_of, try_get_local};
use crate::memory::guarded_stack::STACK_GUARD_PAGES;
use core::sync::atomic::Ordering;
use x86_64::registers::control::Cr2;
use x86_64::structures::idt::{InterruptStackFrame, PageFaultErrorCode};
use crate::interrupt::nmi_handler_state::{NmiHandlerState, NMI_HANDLER_STATES};
use crate::task::local_scheduler::schedule;
use crate::time::on_timer_tick;

pub extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    let accessed_address = Cr2::read().unwrap();
    if let Some(stack) = STACK_GUARD_PAGES
        .lock()
        .iter()
        .find_map(|(page, stack_id)| {
            if accessed_address.align_down(page.size().byte_len_u64()) == page.start_addr() {
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

pub extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {

    let cpu = get_local();
    let mut local_apic = cpu.local_apic.get().unwrap().try_lock().unwrap();
    // Safety: The interrupt finished, send eoi
    unsafe { local_apic.end_of_interrupt() };

    on_timer_tick();
    schedule(cpu);
}

// -- NMI ---
pub fn handle_panic_from_other_cpu() -> ! {
    if let Some(local) = try_get_local()
        && let Some(mut local_apic) = local
        .local_apic
        .get()
        .and_then(|local_apic| local_apic.try_lock())
        && let Some(nmi_handler_states) = NMI_HANDLER_STATES.get()
    {
        for (cpu_id, nmi_handler_state) in nmi_handler_states
            .iter()
            .enumerate()
            // Send NMI except this cpu
            .filter(|(cpu_id, _)| *cpu_id as u32 != local.kernel_id)
        {
            if let NmiHandlerState::NmiHandlerSet =
                nmi_handler_state.swap(NmiHandlerState::KernelPanicked, Ordering::Release)
            {
                // Hlt other cpus
                unsafe { local_apic.send_nmi(local_apic_id_of(cpu_id as u32)) };
            }
        }
    }
    hlt_loop()
}