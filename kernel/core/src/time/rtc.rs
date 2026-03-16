use x86_64::instructions::port::Port;

fn read_rtc_register(reg: u8) -> u8 {
    unsafe {
        Port::<u8>::new(0x70).write(reg);
        Port::<u8>::new(0x71).read()
    }
}

fn is_update_in_progress() -> bool {
    read_rtc_register(0x0A) & 0x80 != 0
}

fn bcd_to_bin(bcd: u8) -> u8 {
    (bcd & 0x0F) + (bcd >> 4) * 10
}

/// Read the RTC and return Unix time in whole seconds since the epoch (1970-01-01 00:00:00 UTC).
///
/// Waits for any in-progress RTC update to finish before reading so values are consistent.
/// Handles both BCD and binary RTC modes (detected from status register B).
pub fn read_unix_timestamp() -> u64 {
    // Wait until the RTC is not mid-update.
    while is_update_in_progress() {}

    let raw_sec  = read_rtc_register(0x00);
    let raw_min  = read_rtc_register(0x02);
    let raw_hour = read_rtc_register(0x04);
    let raw_day  = read_rtc_register(0x07);
    let raw_mon  = read_rtc_register(0x08);
    let raw_year = read_rtc_register(0x09);
    let raw_cent = read_rtc_register(0x32); // CMOS century (may be unreliable on some HW)

    // Status register B bit 2: 0 = BCD, 1 = binary.
    let is_binary = read_rtc_register(0x0B) & 0x04 != 0;

    let decode = |v: u8| if is_binary { v } else { bcd_to_bin(v) };
    let decode_cent = |v: u8| if is_binary { v } else { bcd_to_bin(v) };

    let sec  = decode(raw_sec)  as u32;
    let min  = decode(raw_min)  as u32;
    let hour = decode(raw_hour) as u32;
    let day  = decode(raw_day)  as u32;
    let mon  = decode(raw_mon)  as u32;
    let year = decode(raw_year) as u32;
    let cent = decode_cent(raw_cent);

    // Determine the full 4-digit year.
    // The century register is valid on most UEFI/ACPI systems (typically 0x32 or 0x37).
    let full_year = if (19..=21).contains(&cent) {
        cent as u32 * 100 + year
    } else {
        // Century register not available or garbage: use Y2K pivot (year >= 70 → 1900s).
        if year >= 70 { 1900 + year } else { 2000 + year }
    };

    log::info!(
        "RTC: {:04}-{:02}-{:02} {:02}:{:02}:{:02} (century reg={})",
        full_year, mon, day, hour, min, sec, cent
    );

    to_unix_secs(full_year, mon, day, hour, min, sec)
}

fn is_leap(year: u32) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

fn days_in_month(m: u32, year: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => if is_leap(year) { 29 } else { 28 },
        _ => 28,
    }
}

fn to_unix_secs(year: u32, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> u64 {
    let mut days: u64 = 0;
    for y in 1970..year {
        days += if is_leap(y) { 366 } else { 365 };
    }
    for m in 1..month {
        days += days_in_month(m, year) as u64;
    }
    days += day as u64 - 1; // day is 1-based
    days * 86_400 + hour as u64 * 3_600 + min as u64 * 60 + sec as u64
}
