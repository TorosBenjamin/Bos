pub const HIGHER_HALF_START: u64 = 0xFFFF800000000000;
/// Last canonical address in the lower half (inclusive).
/// In 48-bit virtual addressing: bits 63:47 must all be 0 for the lower half.
pub const LOWER_HALF_END: u64 = 0x7FFFFFFFFFFF;

pub const USER_MIN: u64 = 0x1000;
pub const USER_MAX: u64 = LOWER_HALF_END;

// Apic timer
pub const APIC_TIMER_DISABLE: u32 = 1 << 16;
pub const APIC_TIMER_MODE_ONESHOT: u32 = 0b00 << 17;
pub const APIC_TIMER_MODE_PERIODIC: u32 = 0b01 << 17;
pub const APIC_TIMER_MODE_TSC_DEADLINE: u32 = 0b10 << 17;
