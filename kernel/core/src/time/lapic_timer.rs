use core::sync::atomic::Ordering;
use x86::msr::{wrmsr, IA32_TSC_DEADLINE, IA32_X2APIC_DIV_CONF, IA32_X2APIC_ESR, IA32_X2APIC_LVT_THERMAL, IA32_X2APIC_LVT_TIMER};
use crate::consts::{APIC_TIMER_MODE_TSC_DEADLINE};
use crate::interrupt::InterruptVector;
use crate::time::tsc::{value, TSC_HZ};

#[derive(Clone, Copy, Debug)]
#[repr(u32)]
pub enum LapicTimerDivide {
    By1   = 1,
    By2   = 2,
    By4   = 4,
    By8   = 8,
    By16  = 16,
    By32  = 32,
    By64  = 64,
    By128 = 128,
}

impl LapicTimerDivide {
    fn as_register_value(&self) -> u32 {
        // Note: bit 2 is always 0.
        match self {
            Self::By1   => 0b1011,
            Self::By2   => 0b0000,
            Self::By4   => 0b0001,
            Self::By8   => 0b0010,
            Self::By16  => 0b0011,
            Self::By32  => 0b1000,
            Self::By64  => 0b1001,
            Self::By128 => 0b1010,
        }
    }
}



pub fn enable() {
    let timer_enable = u8::from(InterruptVector::LocalApicTimer) as u32 | APIC_TIMER_MODE_TSC_DEADLINE;
    unsafe {
        wrmsr(IA32_X2APIC_LVT_TIMER, timer_enable as u64);
    }
}

pub fn set_deadline(nanoseconds: u64) {
    let tsc_hz = TSC_HZ.load(Ordering::SeqCst);
    let ticks = (nanoseconds * tsc_hz) / 1_000_000;
    unsafe {
        wrmsr(IA32_TSC_DEADLINE, value() + ticks);
    }
}

/// Set lapic timer into tsc deadline timer mode
pub fn init() {
    // For now only hande X2Apic
    unsafe {
        wrmsr(IA32_X2APIC_DIV_CONF, LapicTimerDivide::By16.as_register_value() as u64);

        // map X2APIC timer to the `LocalApicTimer` interrupt handler in the IDT
        wrmsr(IA32_X2APIC_LVT_TIMER, u8::from(InterruptVector::LocalApicTimer) as u64 | APIC_TIMER_MODE_TSC_DEADLINE as u64);

        wrmsr(IA32_X2APIC_LVT_THERMAL, 1 << 16); // masked — vector 0 would fire as #DE
        wrmsr(IA32_X2APIC_ESR, 0);
    }
}