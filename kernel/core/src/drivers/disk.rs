//! IDE PIO driver — primary channel, master drive, LBA28.
//!
//! No IRQs are used; every operation polls BSY/DRQ.  This is sufficient for a
//! single-threaded filesystem server that issues one sector request at a time.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use x86_64::instructions::port::{Port, PortReadOnly};

// Primary IDE channel I/O ports
const DATA:       u16 = 0x1F0;
const SECTOR_COUNT: u16 = 0x1F2;
const LBA_LO:     u16 = 0x1F3;
const LBA_MID:    u16 = 0x1F4;
const LBA_HI:     u16 = 0x1F5;
const DRIVE_HEAD: u16 = 0x1F6;
const STATUS_CMD: u16 = 0x1F7; // write = command, read = status
const ALT_STATUS: u16 = 0x3F6; // read = alternate status (doesn't clear IRQ)

// Status bits
const BSY: u8 = 0x80;
const DRQ: u8 = 0x08;
const ERR: u8 = 0x01;

// ATA commands
const CMD_IDENTIFY:     u8 = 0xEC;
const CMD_READ_SECTORS: u8 = 0x20;
const CMD_WRITE_SECTORS: u8 = 0x30;
const CMD_CACHE_FLUSH:  u8 = 0xE7;

pub static DISK_PRESENT:     AtomicBool = AtomicBool::new(false);
pub static DISK_SECTOR_COUNT: AtomicU64  = AtomicU64::new(0);

/// Spin until BSY clears, then return the status byte.
#[inline(always)]
unsafe fn wait_not_busy() -> u8 {
    let mut port: PortReadOnly<u8> = PortReadOnly::new(STATUS_CMD);
    loop {
        let s = unsafe { port.read() };
        if s & BSY == 0 {
            return s;
        }
        core::hint::spin_loop();
    }
}

/// Read the alternate status 4 times (≈ 400 ns delay).
#[inline(always)]
unsafe fn io_delay() {
    let mut port: PortReadOnly<u8> = PortReadOnly::new(ALT_STATUS);
    for _ in 0..4 {
        unsafe { let _ = port.read(); }
    }
}

pub fn init() {
    unsafe {
        // Select master drive on primary channel
        let mut drive_port: Port<u8> = Port::new(DRIVE_HEAD);
        drive_port.write(0xA0);  // master, CHS mode for IDENTIFY
        io_delay();

        // Clear LBA registers
        Port::<u8>::new(SECTOR_COUNT).write(0);
        Port::<u8>::new(LBA_LO).write(0);
        Port::<u8>::new(LBA_MID).write(0);
        Port::<u8>::new(LBA_HI).write(0);

        // Send IDENTIFY
        Port::<u8>::new(STATUS_CMD).write(CMD_IDENTIFY);
        io_delay();

        // Read status — if 0, no drive present
        let status = PortReadOnly::<u8>::new(STATUS_CMD).read();
        if status == 0 {
            log::info!("disk::init: no IDE drive detected");
            return;
        }

        // Wait for BSY to clear; poll until DRQ or ERR
        let status = wait_not_busy();
        if status & ERR != 0 {
            log::warn!("disk::init: IDENTIFY returned ERR — not a plain ATA drive");
            return;
        }

        // Wait for DRQ
        loop {
            let s = PortReadOnly::<u8>::new(STATUS_CMD).read();
            if s & DRQ != 0 { break; }
            if s & ERR != 0 { return; }
            core::hint::spin_loop();
        }

        // Read 256 u16 IDENTIFY words
        let mut words = [0u16; 256];
        let mut data_port: Port<u16> = Port::new(DATA);
        for w in words.iter_mut() {
            *w = data_port.read();
        }

        // Words 60-61: 28-bit LBA sector count
        let lba28_sectors = ((words[61] as u64) << 16) | (words[60] as u64);
        // Words 100-103: 48-bit LBA sector count (if supported)
        let lba48_sectors =
            ((words[103] as u64) << 48)
            | ((words[102] as u64) << 32)
            | ((words[101] as u64) << 16)
            | (words[100] as u64);

        let total = if lba48_sectors > 0 { lba48_sectors } else { lba28_sectors };

        DISK_PRESENT.store(true, Ordering::Release);
        DISK_SECTOR_COUNT.store(total, Ordering::Relaxed);
        log::info!("disk::init: ATA drive found, {} sectors ({} MB)", total, total / 2048);
    }
}

/// Read `count` 512-byte sectors starting at `lba` into `buf`.
/// Returns `true` on success.
pub fn read_sectors(lba: u64, count: u32, buf: &mut [u8]) -> bool {
    if !DISK_PRESENT.load(Ordering::Acquire) {
        return false;
    }
    if buf.len() < (count as usize) * 512 {
        return false;
    }

    unsafe {
        // Select master drive, LBA mode, top 4 bits of LBA28
        Port::<u8>::new(DRIVE_HEAD).write(0xE0 | (((lba >> 24) & 0x0F) as u8));
        io_delay();
        wait_not_busy();

        Port::<u8>::new(SECTOR_COUNT).write(count as u8);
        Port::<u8>::new(LBA_LO).write( (lba       & 0xFF) as u8);
        Port::<u8>::new(LBA_MID).write(((lba >> 8 ) & 0xFF) as u8);
        Port::<u8>::new(LBA_HI).write( ((lba >> 16) & 0xFF) as u8);
        Port::<u8>::new(STATUS_CMD).write(CMD_READ_SECTORS);

        let mut data_port: Port<u16> = Port::new(DATA);

        for sector in 0..(count as usize) {
            // Wait for DRQ
            loop {
                let s = wait_not_busy();
                if s & DRQ != 0 { break; }
                if s & ERR != 0 { return false; }
            }

            let offset = sector * 512;
            let chunk = &mut buf[offset..offset + 512];
            // Read 256 u16 words into the byte slice
            for i in 0..256 {
                let word = data_port.read();
                chunk[i * 2]     = (word & 0xFF) as u8;
                chunk[i * 2 + 1] = (word >> 8)   as u8;
            }
        }
    }
    true
}

/// Write `count` 512-byte sectors starting at `lba` from `buf`.
/// Returns `true` on success.
pub fn write_sectors(lba: u64, count: u32, buf: &[u8]) -> bool {
    if !DISK_PRESENT.load(Ordering::Acquire) {
        return false;
    }
    if buf.len() < (count as usize) * 512 {
        return false;
    }

    unsafe {
        Port::<u8>::new(DRIVE_HEAD).write(0xE0 | (((lba >> 24) & 0x0F) as u8));
        io_delay();
        wait_not_busy();

        Port::<u8>::new(SECTOR_COUNT).write(count as u8);
        Port::<u8>::new(LBA_LO).write( (lba       & 0xFF) as u8);
        Port::<u8>::new(LBA_MID).write(((lba >> 8 ) & 0xFF) as u8);
        Port::<u8>::new(LBA_HI).write( ((lba >> 16) & 0xFF) as u8);
        Port::<u8>::new(STATUS_CMD).write(CMD_WRITE_SECTORS);

        let mut data_port: Port<u16> = Port::new(DATA);

        for sector in 0..(count as usize) {
            loop {
                let s = wait_not_busy();
                if s & DRQ != 0 { break; }
                if s & ERR != 0 { return false; }
            }

            let offset = sector * 512;
            let chunk = &buf[offset..offset + 512];
            for i in 0..256 {
                let lo = chunk[i * 2]     as u16;
                let hi = chunk[i * 2 + 1] as u16;
                data_port.write(lo | (hi << 8));
            }
        }

        // Flush write cache
        Port::<u8>::new(STATUS_CMD).write(CMD_CACHE_FLUSH);
        wait_not_busy();
    }
    true
}
