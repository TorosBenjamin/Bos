use core::sync::atomic::Ordering;
use x86_64::structures::idt::InterruptDescriptorTable;
use x86_64::VirtAddr;
use crate::gdt::IstStackIndexes;
use crate::interrupt::handlers::{breakpoint_handler, double_fault_handler, handle_panic_from_other_cpu, nmi_handler, page_fault_handler, timer_interrupt_handler};
use crate::interrupt::InterruptVector;
use crate::interrupt::nmi_handler_state::{NmiHandlerState, NMI_HANDLER_STATES};
use crate::memory::cpu_local_data::get_local;

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
        // Timer interrupt must NOT use IST: each task's interrupt frame must be
        // saved on its own stack so it persists across context switches. IST would
        // put all frames on a single shared stack that gets overwritten each tick.
        unsafe {
            idt[u8::from(InterruptVector::LocalApicTimer)]
                .set_handler_addr(VirtAddr::new(timer_interrupt_handler as u64));
        }
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
