use core::sync::atomic::Ordering;
use x86::msr::{rdmsr, wrmsr, IA32_TSC_DEADLINE, IA32_X2APIC_CUR_COUNT, IA32_X2APIC_DIV_CONF, IA32_X2APIC_ESR, IA32_X2APIC_INIT_COUNT, IA32_X2APIC_LVT_THERMAL, IA32_X2APIC_LVT_TIMER};
use x86_64::registers::model_specific::Msr;
use crate::consts::{APIC_TIMER_DISABLE, APIC_TIMER_MODE_PERIODIC};
use crate::interrupt::InterruptVector;
use crate::memory::cpu_local_data::get_local;
use crate::time::{pit, tsc};
use crate::time::tsc::{TSC_HZ};

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



/// Returns the number of apic ticks in the given microseconds
fn calibrate_with_pit(qs: u32) -> u32 {
    // Start with the max counter value, since we're counting down
    const INITIAL_COUNT: u32 = 0xFFFF_FFFF;

    unsafe {
        wrmsr(IA32_X2APIC_DIV_CONF, LapicTimerDivide::By16.as_register_value() as u64);
        wrmsr(IA32_X2APIC_INIT_COUNT, INITIAL_COUNT as u64);
    }

    // wait for the given period using the PIT clock
    pit::sleep_qs(qs).unwrap();

    let end_count = unsafe {
        // stop apic timer
        wrmsr(IA32_X2APIC_LVT_TIMER, APIC_TIMER_DISABLE as u64);
        rdmsr(IA32_X2APIC_CUR_COUNT) as u32
    };

    INITIAL_COUNT - end_count
}

pub fn enable() {
    let timer_enable = LOCAL_APIC_LVT_IRQ as u32 | APIC_TIMER_MODE_PERIODIC;
    unsafe {
        wrmsr(IA32_X2APIC_LVT_TIMER, timer_enable as u64);
        wrmsr(IA32_X2APIC_INIT_COUNT, self.initial_timer_count as u64);
    }
}

/// Set lapic timer into tsc deadline timer mode
pub fn init() {
    let apic_period = calibrate_with_pit(10000);

    // For now only hande X2Apic
    unsafe {
        wrmsr(IA32_X2APIC_DIV_CONF, LapicTimerDivide::By16.as_register_value() as u64);

        // map X2APIC timer to the `LOCAL_APIC_LVT_IRQ` interrupt handler in the IDT
        wrmsr(IA32_X2APIC_LVT_TIMER, InterruptVector::LocalApicTimer as u64 | APIC_TIMER_MODE_PERIODIC as u64);
        wrmsr(IA32_X2APIC_INIT_COUNT, apic_period as u64);

        wrmsr(IA32_X2APIC_LVT_THERMAL, 0);
        wrmsr(IA32_X2APIC_ESR, 0);


        wrmsr(IA32_X2APIC_DIV_CONF, LapicTimerDivide::By16.as_register_value() as u64);
    }
}