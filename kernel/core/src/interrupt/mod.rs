use num_enum::IntoPrimitive;

pub mod idt;
pub mod nmi_handler_state;
pub mod handlers;

#[derive(Debug, IntoPrimitive)]
#[repr(u8)]
pub enum InterruptVector {
    LocalApicSpurious = 0x20,
    LocalApicTimer,       // 0x21
    LocalApicError,       // 0x22
    Keyboard = 0x23,
    Reschedule = 0x24,
}