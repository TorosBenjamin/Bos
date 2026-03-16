//! IDE disk driver — primary channel, master drive, LBA28.
//!
//! Supports both PIO and Bus-Mastering DMA reads.  DMA is used automatically
//! when the IDE controller's Bus Master interface is detected via PCI.
//! DMA eliminates per-word port I/O, reducing VM exits dramatically on QEMU.

use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use x86_64::instructions::port::{Port, PortReadOnly};
use crate::drivers::pci;
use crate::memory::MEMORY;
use crate::memory::hhdm_offset::hhdm_offset;
use crate::memory::physical_memory::MemoryType;
use x86_64::structures::paging::{Size4KiB, PageSize};

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
const CMD_READ_DMA:     u8 = 0xC8;
const CMD_WRITE_SECTORS: u8 = 0x30;
const CMD_CACHE_FLUSH:  u8 = 0xE7;

// Bus Master register offsets (from BM_BASE)
const BM_CMD:    u16 = 0x00;
const BM_STATUS: u16 = 0x02;
const BM_PRDT:   u16 = 0x04;

// Bus Master Status bits
const BM_STAT_ACTIVE: u8 = 0x01;
const BM_STAT_ERROR:  u8 = 0x02;
const BM_STAT_IRQ:    u8 = 0x04;

/// Maximum number of DMA buffer pages (32 pages = 128KB = 256 sectors max).
const MAX_DMA_PAGES: usize = 32;

pub static DISK_PRESENT:     AtomicBool = AtomicBool::new(false);
pub static DISK_SECTOR_COUNT: AtomicU64  = AtomicU64::new(0);

/// Bus Master I/O base address (0 = DMA not available).
static BM_BASE: AtomicU16 = AtomicU16::new(0);

/// Set by the IRQ 14 handler when a DMA transfer completes.
static DMA_IRQ_FIRED: AtomicBool = AtomicBool::new(false);

/// Physical address of the PRDT page.
static PRDT_PHYS: AtomicU64 = AtomicU64::new(0);

/// Physical addresses of pre-allocated DMA buffer pages.
static DMA_BUF_PHYS: spin::Once<[u64; MAX_DMA_PAGES]> = spin::Once::new();

/// Maximum iterations to spin waiting for BSY or DRQ.
const POLL_TIMEOUT: usize = 5_000_000;

/// Spin until BSY clears, then return `Some(status)`.
#[inline(always)]
unsafe fn try_wait_not_busy() -> Option<u8> {
    let mut port: PortReadOnly<u8> = PortReadOnly::new(STATUS_CMD);
    for _ in 0..POLL_TIMEOUT {
        let s = unsafe { port.read() };
        if s & BSY == 0 {
            return Some(s);
        }
        core::hint::spin_loop();
    }
    None
}

/// Read the alternate status 4 times (≈ 400 ns delay).
#[inline(always)]
unsafe fn io_delay() {
    let mut port: PortReadOnly<u8> = PortReadOnly::new(ALT_STATUS);
    for _ in 0..4 {
        unsafe { let _ = port.read(); }
    }
}

/// Called from the IRQ 14 handler.  Clears the Bus Master interrupt bit
/// and reads ATA status to deassert the IRQ at the drive level.
pub fn on_ata_interrupt() {
    let bm = BM_BASE.load(Ordering::Relaxed);
    if bm != 0 {
        unsafe {
            // Read BM Status to check interrupt bit
            let status = PortReadOnly::<u8>::new(bm + BM_STATUS).read();
            if status & BM_STAT_IRQ != 0 {
                // Clear Interrupt + Error bits (write-1-to-clear)
                Port::<u8>::new(bm + BM_STATUS).write(BM_STAT_IRQ | BM_STAT_ERROR);
            }
            // Read ATA status to clear the drive's IRQ
            let _ = PortReadOnly::<u8>::new(STATUS_CMD).read();
        }
    } else {
        // No DMA — still need to clear ATA status to deassert IRQ
        unsafe { let _ = PortReadOnly::<u8>::new(STATUS_CMD).read(); }
    }
    DMA_IRQ_FIRED.store(true, Ordering::Release);
}

/// Discover IDE Bus Master via PCI, allocate PRDT and DMA buffer pages.
fn init_dma() {
    // Scan PCI for IDE controller (class 0x01, subclass 0x01).
    // QEMU PIIX3 is typically at bus 0, device 1, function 1.
    let mut found = None;
    'scan: for bus in 0u8..=255 {
        for dev in 0u8..32 {
            let vendor = pci::config_read(bus, dev, 0, 0x00, 2).unwrap_or(0xFFFF);
            if vendor == 0xFFFF { continue; }

            for func in 0u8..8 {
                let class_reg = pci::config_read(bus, dev, func, 0x08, 4).unwrap_or(0);
                let class = ((class_reg >> 24) & 0xFF) as u8;
                let subclass = ((class_reg >> 16) & 0xFF) as u8;
                if class == 0x01 && subclass == 0x01 {
                    found = Some((bus, dev, func));
                    break 'scan;
                }
                // If not multi-function device, skip functions 1-7
                if func == 0 {
                    let header = pci::config_read(bus, dev, 0, 0x0E, 1).unwrap_or(0);
                    if header & 0x80 == 0 { break; }
                }
            }
        }
    }

    let (bus, dev, func) = match found {
        Some(bdf) => bdf,
        None => {
            log::info!("disk: no PCI IDE controller found, DMA not available");
            return;
        }
    };

    // Read BAR4 (Bus Master Base Address) — offset 0x20
    let bar4 = pci::config_read(bus, dev, func, 0x20, 4).unwrap_or(0);
    if bar4 == 0 || bar4 & 1 == 0 {
        log::info!("disk: IDE controller has no Bus Master BAR, DMA not available");
        return;
    }
    let bm_base = (bar4 & 0xFFFC) as u16;

    // Enable bus mastering + I/O space in PCI Command register
    let cmd = pci::config_read(bus, dev, func, 0x04, 2).unwrap_or(0);
    pci::config_write(bus, dev, func, 0x04, 2, cmd | 0x05); // bit 0 = I/O, bit 2 = Bus Master

    // Allocate PRDT page and DMA buffer pages
    let memory = MEMORY.get().unwrap();
    let mut pm = memory.physical_memory.lock();

    let prdt_frame = match pm.allocate_frame_with_type(MemoryType::UsedByKernel(
        crate::memory::physical_memory::KernelMemoryUsageType::PageTables,
    )) {
        Some(f) => f,
        None => {
            log::warn!("disk: failed to allocate PRDT page for DMA");
            return;
        }
    };
    let prdt_phys = prdt_frame.start_address().as_u64();

    // Zero the PRDT page
    let hhdm = hhdm_offset().as_u64();
    unsafe {
        core::ptr::write_bytes((hhdm + prdt_phys) as *mut u8, 0, 4096);
    }

    let mut buf_phys = [0u64; MAX_DMA_PAGES];
    for slot in buf_phys.iter_mut() {
        let frame = match pm.allocate_frame_with_type(MemoryType::UsedByKernel(
            crate::memory::physical_memory::KernelMemoryUsageType::PageTables,
        )) {
            Some(f) => f,
            None => {
                log::warn!("disk: failed to allocate DMA buffer page");
                return;
            }
        };
        *slot = frame.start_address().as_u64();
    }
    drop(pm);

    PRDT_PHYS.store(prdt_phys, Ordering::Relaxed);
    DMA_BUF_PHYS.call_once(|| buf_phys);
    BM_BASE.store(bm_base, Ordering::Release);

    log::info!(
        "disk: Bus Master DMA enabled (BM base={:#x}, PCI {:02x}:{:02x}.{:x})",
        bm_base, bus, dev, func,
    );
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
        let status = match try_wait_not_busy() {
            Some(s) => s,
            None => {
                log::warn!("disk::init: timeout waiting for BSY to clear after IDENTIFY");
                return;
            }
        };
        if status & ERR != 0 {
            log::warn!("disk::init: IDENTIFY returned ERR — not a plain ATA drive");
            return;
        }

        // Wait for DRQ
        let mut drq_ready = false;
        for _ in 0..POLL_TIMEOUT {
            let s = PortReadOnly::<u8>::new(STATUS_CMD).read();
            if s & DRQ != 0 { drq_ready = true; break; }
            if s & ERR != 0 { return; }
            core::hint::spin_loop();
        }
        if !drq_ready {
            log::warn!("disk::init: timeout waiting for DRQ after IDENTIFY");
            return;
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

    // Try to set up DMA after basic ATA init
    if DISK_PRESENT.load(Ordering::Acquire) {
        init_dma();
    }
}

/// Read sectors using Bus Master DMA.  Returns true on success.
fn read_sectors_dma(lba: u64, count: u32, buf: &mut [u8]) -> bool {
    let bm = BM_BASE.load(Ordering::Acquire);
    if bm == 0 { return false; }

    let prdt_phys = PRDT_PHYS.load(Ordering::Relaxed);
    let buf_phys = match DMA_BUF_PHYS.get() {
        Some(p) => p,
        None => return false,
    };

    let total_bytes = count as usize * 512;
    let n_pages = total_bytes.div_ceil(Size4KiB::SIZE as usize);
    if n_pages > MAX_DMA_PAGES { return false; }

    let hhdm = hhdm_offset().as_u64();

    // Build PRDT entries
    let prdt_virt = (hhdm + prdt_phys) as *mut [u32; 2];
    for i in 0..n_pages {
        let bytes_this_page = if i == n_pages - 1 {
            total_bytes - i * Size4KiB::SIZE as usize
        } else {
            Size4KiB::SIZE as usize
        };
        let phys = buf_phys[i] as u32;
        let byte_count = if bytes_this_page == 0x10000 { 0u16 } else { bytes_this_page as u16 };
        let eot: u32 = if i == n_pages - 1 { 0x8000_0000 } else { 0 };
        unsafe {
            let entry = prdt_virt.add(i);
            (*entry)[0] = phys;
            (*entry)[1] = (byte_count as u32) | eot;
        }
    }

    unsafe {
        // Stop any prior transfer
        Port::<u8>::new(bm + BM_CMD).write(0);
        // Clear status bits (write-1-to-clear Interrupt + Error)
        Port::<u8>::new(bm + BM_STATUS).write(BM_STAT_IRQ | BM_STAT_ERROR);
        // Write PRDT physical address
        Port::<u32>::new(bm + BM_PRDT).write(prdt_phys as u32);

        // Issue ATA READ DMA command
        Port::<u8>::new(DRIVE_HEAD).write(0xE0 | (((lba >> 24) & 0x0F) as u8));
        io_delay();
        if try_wait_not_busy().is_none() {
            log::warn!("disk: DMA read timeout waiting for BSY (lba={lba})");
            return false;
        }
        Port::<u8>::new(SECTOR_COUNT).write(count as u8);
        Port::<u8>::new(LBA_LO).write( (lba       & 0xFF) as u8);
        Port::<u8>::new(LBA_MID).write(((lba >> 8 ) & 0xFF) as u8);
        Port::<u8>::new(LBA_HI).write( ((lba >> 16) & 0xFF) as u8);

        DMA_IRQ_FIRED.store(false, Ordering::Release);
        Port::<u8>::new(STATUS_CMD).write(CMD_READ_DMA);

        // Start Bus Master: bit 0 = Start, bit 3 = direction (1 = read from device)
        Port::<u8>::new(bm + BM_CMD).write(0x09);

        // Spin-poll BM Status for completion.
        // In QEMU the transfer completes nearly instantly. On real hardware
        // this would ideally use the IRQ to sleep/wake.
        let mut ok = false;
        for _ in 0..POLL_TIMEOUT {
            let status = PortReadOnly::<u8>::new(bm + BM_STATUS).read();
            if status & BM_STAT_ERROR != 0 {
                log::warn!("disk: DMA read error (lba={lba})");
                break;
            }
            if status & BM_STAT_ACTIVE == 0 {
                // Transfer complete
                ok = true;
                break;
            }
            core::hint::spin_loop();
        }

        // Stop Bus Master
        Port::<u8>::new(bm + BM_CMD).write(0);
        // Clear status
        Port::<u8>::new(bm + BM_STATUS).write(BM_STAT_IRQ | BM_STAT_ERROR);

        if !ok {
            return false;
        }

        // Copy data from DMA buffer pages to caller's buffer
        let mut copied = 0usize;
        for i in 0..n_pages {
            let src = (hhdm + buf_phys[i]) as *const u8;
            let chunk = (total_bytes - copied).min(Size4KiB::SIZE as usize);
            core::ptr::copy_nonoverlapping(src, buf.as_mut_ptr().add(copied), chunk);
            copied += chunk;
        }
    }

    true
}

/// Read `count` 512-byte sectors starting at `lba` into `buf`.
/// Uses DMA when available, falls back to PIO.
pub fn read_sectors(lba: u64, count: u32, buf: &mut [u8]) -> bool {
    if !DISK_PRESENT.load(Ordering::Acquire) {
        return false;
    }
    if buf.len() < (count as usize) * 512 {
        return false;
    }

    // Try DMA first
    if BM_BASE.load(Ordering::Acquire) != 0 {
        return read_sectors_dma(lba, count, buf);
    }

    // PIO fallback
    read_sectors_pio(lba, count, buf)
}

/// PIO read path (original implementation).
fn read_sectors_pio(lba: u64, count: u32, buf: &mut [u8]) -> bool {
    unsafe {
        Port::<u8>::new(DRIVE_HEAD).write(0xE0 | (((lba >> 24) & 0x0F) as u8));
        io_delay();
        if try_wait_not_busy().is_none() {
            log::warn!("disk: read_sectors timeout waiting for BSY (lba={lba})");
            return false;
        }

        Port::<u8>::new(SECTOR_COUNT).write(count as u8);
        Port::<u8>::new(LBA_LO).write( (lba       & 0xFF) as u8);
        Port::<u8>::new(LBA_MID).write(((lba >> 8 ) & 0xFF) as u8);
        Port::<u8>::new(LBA_HI).write( ((lba >> 16) & 0xFF) as u8);
        Port::<u8>::new(STATUS_CMD).write(CMD_READ_SECTORS);

        let mut data_port: Port<u16> = Port::new(DATA);

        for sector in 0..(count as usize) {
            let s = match try_wait_not_busy() {
                Some(s) => s,
                None => {
                    log::warn!("disk: read_sectors timeout waiting for DRQ (lba={lba} sector={sector})");
                    return false;
                }
            };
            if s & DRQ == 0 { return false; }
            if s & ERR != 0 { return false; }

            let offset = sector * 512;
            let chunk = &mut buf[offset..offset + 512];
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
        if try_wait_not_busy().is_none() {
            log::warn!("disk: write_sectors timeout waiting for BSY (lba={lba})");
            return false;
        }

        Port::<u8>::new(SECTOR_COUNT).write(count as u8);
        Port::<u8>::new(LBA_LO).write( (lba       & 0xFF) as u8);
        Port::<u8>::new(LBA_MID).write(((lba >> 8 ) & 0xFF) as u8);
        Port::<u8>::new(LBA_HI).write( ((lba >> 16) & 0xFF) as u8);
        Port::<u8>::new(STATUS_CMD).write(CMD_WRITE_SECTORS);

        let mut data_port: Port<u16> = Port::new(DATA);

        for sector in 0..(count as usize) {
            let s = match try_wait_not_busy() {
                Some(s) => s,
                None => {
                    log::warn!("disk: write_sectors timeout waiting for DRQ (lba={lba} sector={sector})");
                    return false;
                }
            };
            if s & DRQ == 0 { return false; }
            if s & ERR != 0 { return false; }

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
        if try_wait_not_busy().is_none() {
            log::warn!("disk: write_sectors timeout waiting for cache flush (lba={lba})");
            return false;
        }
    }
    true
}
