use x2apic::lapic::{TimerDivide, TimerMode};
use crate::memory::cpu_local_data::get_local;
use crate::time::pit;

fn calibrate_lapic_timer() -> u32 {
    let mut lapic = get_local()
        .local_apic
        .get()
        .unwrap()
        .try_lock()
        .unwrap();

    unsafe {
        lapic.set_timer_mode(TimerMode::OneShot);
        lapic.set_timer_divide(TimerDivide::Div1);
        lapic.set_timer_initial(u32::MAX);
        lapic.enable_timer();
    }

    // Wait 10 ms using PIT
    pit::sleep_ms(10);

    let remaining = unsafe{ lapic.timer_current() };
    let elapsed = u32::MAX - remaining;

    elapsed / 10 // ticks per millisecond
}


pub fn init() {
    let ticks_per_ms = calibrate_lapic_timer();

    let mut lapic = get_local()
        .local_apic
        .get()
        .unwrap()
        .try_lock()
        .unwrap();

    unsafe {
        lapic.set_timer_mode(TimerMode::Periodic);
        lapic.set_timer_divide(TimerDivide::Div1);
        lapic.set_timer_initial(ticks_per_ms);
        lapic.enable_timer();
    }
}