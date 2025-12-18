use crate::gdt::IstStackIndexes;
use crate::hlt_loop;
use crate::memory::cpu_local_data::{get_local, local_apic_id_of, try_get_local};
use crate::memory::guarded_stack::STACK_GUARD_PAGES;
use crate::nmi_handler_state::{NMI_HANDLER_STATES, NmiHandlerState};
use core::sync::atomic::Ordering;
use x86_64::registers::control::Cr2;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

extern "x86-interrupt" fn page_fault_handler(
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

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) -> ! {
    panic!("Double Fault! Stack frame: {stack_frame:#?}. Error code: {error_code}.")
}

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    log::info!("Breakpoint! Stack frame: {stack_frame:#?}");
}

// -- NMI ---
fn handle_panic_from_other_cpu() -> ! {
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

extern "x86-interrupt" fn nmi_handler(_stack_frame: InterruptStackFrame) {
    handle_panic_from_other_cpu()
}

pub fn init() {
    let idt = get_local().idt.call_once(|| {
        let mut idt = InterruptDescriptorTable::new();
        unsafe {
            idt.page_fault
                .set_handler_fn(page_fault_handler)
                .set_stack_index(u8::from(IstStackIndexes::Exception).into())
        };
        unsafe {
            idt.double_fault
                .set_handler_fn(double_fault_handler)
                .set_stack_index(u8::from(IstStackIndexes::Exception).into())
        };
        idt.breakpoint.set_handler_fn(breakpoint_handler);

        idt.non_maskable_interrupt.set_handler_fn(nmi_handler);
        idt
    });
    idt.load();

    // Update state to available to receive NMIs
    let local = get_local();
    if NMI_HANDLER_STATES.get().unwrap()[local.kernel_id as usize]
        .compare_exchange(
            NmiHandlerState::NmiHandlerNotSet,
            NmiHandlerState::NmiHandlerSet,
            Ordering::Relaxed,
            Ordering::Relaxed,
        )
        .is_err()
    {
        // Kernel already panicked
        handle_panic_from_other_cpu()
    }
}
