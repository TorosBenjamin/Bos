use x86_64::instructions::port::Port;
use crate::consts::{PIT_CH0, PIT_CMD, PIT_FREQ};

/// Busy-wait sleep using PIT (ONLY for calibration)
pub fn sleep_ms(ms: u32) {
    let ticks = (PIT_FREQ / 1000) * ms;
    assert!(ticks <= 0xFFFF);

    let mut cmd = Port::<u8>::new(PIT_CMD);
    let mut ch0 = Port::<u8>::new(PIT_CH0);

    unsafe {
        // Channel 0 | lobyte/hibyte | mode 0 (one-shot) | binary
        cmd.write(0b0011_0000);

        ch0.write((ticks & 0xFF) as u8);
        ch0.write((ticks >> 8) as u8);
    }

    loop {
        let count: u16;

        unsafe {
            // Latch channel 0 count
            cmd.write(0b0000_0000);

            let lo: u8 = ch0.read();
            let hi: u8 = ch0.read();

            count = u16::from_le_bytes([lo, hi]);
        }

        if count == 0 {
            break;
        }

        core::hint::spin_loop();
    }
}