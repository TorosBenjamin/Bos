use crate::memory::cpu_local_data::cpus_count;
use alloc::boxed::Box;
use atomic_enum::atomic_enum;
use spin::once::Once;

#[atomic_enum]
pub enum NmiHandlerState {
    NmiHandlerNotSet,
    NmiHandlerSet,
    KernelPanicked,
}

pub static NMI_HANDLER_STATES: Once<Box<[AtomicNmiHandlerState]>> = Once::new();

pub fn init() {
    NMI_HANDLER_STATES.call_once(|| {
        (0..cpus_count())
            .map(|_| AtomicNmiHandlerState::new(NmiHandlerState::NmiHandlerNotSet))
            .collect()
    });
}
