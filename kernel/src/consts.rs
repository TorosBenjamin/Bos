pub const HIGHER_HALF_START: u64 = 0xFFFF800000000000;
pub const LOWER_HALF_END: u64 = 0x800000000000;

pub const USER_MIN: u64 = 0x1000;
pub const USER_MAX: u64 = LOWER_HALF_END - 1;

pub const PIT_FREQ: u32 = 1_193_182;
pub const PIT_CH0: u16 = 0x40;
pub const PIT_CMD: u16 = 0x43;