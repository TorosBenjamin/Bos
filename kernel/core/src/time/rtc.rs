use x86_64::instructions::port::Port;

#[allow(dead_code)]
fn read_rtc_register(register: u8) -> u8 {
    unsafe {
        Port::new(0x70).write(register);
        Port::new(0x71).read()
    }
}

#[allow(dead_code)]
pub fn read_time() -> (u8, u8, u8) {
    let seconds_bcd = read_rtc_register(0x00);
    let minutes_bcd = read_rtc_register(0x02);
    let hours_bcd = read_rtc_register(0x04);

    let seconds = (seconds_bcd & 0x0F) + ((seconds_bcd >> 4) * 10);
    let minutes = (minutes_bcd & 0x0F) + ((minutes_bcd >> 4) * 10);
    let hours = (hours_bcd & 0x0F) + ((hours_bcd >> 4) * 10);

    (hours, minutes, seconds)
}