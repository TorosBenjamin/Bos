use num_enum::IntoPrimitive;

pub mod idt;
pub mod nmi_handler_state;
pub mod handlers;

#[derive(Debug, IntoPrimitive)]
#[repr(u8)]
pub enum InterruptVector {
    LocalApicSpurious = 0x20,
    LocalApicTimer,
    LocalApicError,
}