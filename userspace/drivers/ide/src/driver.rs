//! IDE disk driver — primary channel, master drive, LBA28.
//!
//! Supports both PIO and Bus-Mastering DMA reads.  DMA is used automatically
//! when the IDE controller's Bus Master interface is detected via PCI.
//!
//! Ported from kernel/core/src/drivers/disk.rs to userspace using I/O port syscalls.

use ulib::{inb, inw, outb, outw, outd};

// Primary IDE channel I/O ports
const DATA:         u16 = 0x1F0;
const SECTOR_COUNT: u16 = 0x1F2;
const LBA_LO:       u16 = 0x1F3;
const LBA_MID:      u16 = 0x1F4;
const LBA_HI:       u16 = 0x1F5;
const DRIVE_HEAD:   u16 = 0x1F6;
const STATUS_CMD:   u16 = 0x1F7; // write = command, read = status
const ALT_STATUS:   u16 = 0x3F6; // read = alternate status (doesn't clear IRQ)

// Status bits
const BSY: u8 = 0x80;
const DRQ: u8 = 0x08;
const ERR: u8 = 0x01;

// ATA commands
const CMD_IDENTIFY:      u8 = 0xEC;
const CMD_READ_SECTORS:  u8 = 0x20;
const CMD_READ_DMA:      u8 = 0xC8;
const CMD_WRITE_SECTORS: u8 = 0x30;
const CMD_CACHE_FLUSH:   u8 = 0xE7;

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

const POLL_TIMEOUT: usize = 5_000_000;

pub struct IdeDriver {
    pub sector_count: u64,
    bm_base: u16,
    /// PRDT: virtual addr, physical addr
    prdt: Option<(*mut u8, u64)>,
    /// DMA buffer pages: (virt, phys) pairs
    dma_bufs: Option<[(*mut u8, u64); MAX_DMA_PAGES]>,
}

/// Spin until BSY clears, then return `Some(status)`.
fn try_wait_not_busy() -> Option<u8> {
    for _ in 0..POLL_TIMEOUT {
        let s = inb(STATUS_CMD);
        if s & BSY == 0 {
            return Some(s);
        }
        core::hint::spin_loop();
    }
    None
}

/// Read the alternate status 4 times (≈ 400 ns delay).
fn io_delay() {
    for _ in 0..4 {
        let _ = inb(ALT_STATUS);
    }
}

impl IdeDriver {
    /// Probe the IDE controller and initialize. Returns `None` if no drive found.
    pub fn init() -> Option<Self> {
        // Select master drive
        outb(DRIVE_HEAD, 0xA0);
        io_delay();

        // Clear LBA registers
        outb(SECTOR_COUNT, 0);
        outb(LBA_LO, 0);
        outb(LBA_MID, 0);
        outb(LBA_HI, 0);

        // Send IDENTIFY
        outb(STATUS_CMD, CMD_IDENTIFY);
        io_delay();

        let status = inb(STATUS_CMD);
        if status == 0 {
            return None; // no drive
        }

        let status = try_wait_not_busy()?;
        if status & ERR != 0 {
            return None; // not a plain ATA drive
        }

        // Wait for DRQ
        let mut drq_ready = false;
        for _ in 0..POLL_TIMEOUT {
            let s = inb(STATUS_CMD);
            if s & DRQ != 0 { drq_ready = true; break; }
            if s & ERR != 0 { return None; }
            core::hint::spin_loop();
        }
        if !drq_ready { return None; }

        // Read 256 u16 IDENTIFY words
        let mut words = [0u16; 256];
        for w in words.iter_mut() {
            *w = inw(DATA);
        }

        let lba28_sectors = ((words[61] as u64) << 16) | (words[60] as u64);
        let lba48_sectors =
            ((words[103] as u64) << 48)
            | ((words[102] as u64) << 32)
            | ((words[101] as u64) << 16)
            | (words[100] as u64);

        let sector_count = if lba48_sectors > 0 { lba48_sectors } else { lba28_sectors };

        let mut driver = IdeDriver {
            sector_count,
            bm_base: 0,
            prdt: None,
            dma_bufs: None,
        };

        driver.init_dma();
        Some(driver)
    }

    fn init_dma(&mut self) {
        // Scan PCI for IDE controller (class 0x01, subclass 0x01)
        let mut found = None;
        'scan: for bus in 0u8..=255u8 {
            for dev in 0u8..32 {
                let vendor = ulib::pci_config_read(bus, dev, 0, 0x00, 2).unwrap_or(0xFFFF);
                if vendor == 0xFFFF { continue; }

                for func in 0u8..8 {
                    let class_reg = ulib::pci_config_read(bus, dev, func, 0x08, 4).unwrap_or(0);
                    let class = ((class_reg >> 24) & 0xFF) as u8;
                    let subclass = ((class_reg >> 16) & 0xFF) as u8;
                    if class == 0x01 && subclass == 0x01 {
                        found = Some((bus, dev, func));
                        break 'scan;
                    }
                    if func == 0 {
                        let header = ulib::pci_config_read(bus, dev, 0, 0x0E, 1).unwrap_or(0);
                        if header & 0x80 == 0 { break; }
                    }
                }
            }
        }

        let (bus, dev, func) = match found {
            Some(bdf) => bdf,
            None => return,
        };

        // Read BAR4 (Bus Master Base Address) — offset 0x20
        let bar4 = ulib::pci_config_read(bus, dev, func, 0x20, 4).unwrap_or(0);
        if bar4 == 0 || bar4 & 1 == 0 {
            return; // no Bus Master BAR
        }
        let bm_base = (bar4 & 0xFFFC) as u16;

        // Enable bus mastering + I/O space in PCI Command register
        let cmd = ulib::pci_config_read(bus, dev, func, 0x04, 2).unwrap_or(0);
        ulib::pci_config_write(bus, dev, func, 0x04, 2, cmd | 0x05);

        // Allocate PRDT page via sys_alloc_dma
        let mut prdt_phys: u64 = 0;
        let prdt_virt = ulib::sys_alloc_dma(&mut prdt_phys);
        if prdt_virt.is_null() { return; }
        // Zero the PRDT page
        unsafe { core::ptr::write_bytes(prdt_virt, 0, 4096); }

        // Allocate DMA buffer pages
        let mut bufs = [core::ptr::null_mut::<u8>(); MAX_DMA_PAGES];
        let mut buf_phys = [0u64; MAX_DMA_PAGES];
        for i in 0..MAX_DMA_PAGES {
            bufs[i] = ulib::sys_alloc_dma(&mut buf_phys[i]);
            if bufs[i].is_null() { return; }
        }

        let mut dma_bufs = [(core::ptr::null_mut::<u8>(), 0u64); MAX_DMA_PAGES];
        for i in 0..MAX_DMA_PAGES {
            dma_bufs[i] = (bufs[i], buf_phys[i]);
        }

        self.bm_base = bm_base;
        self.prdt = Some((prdt_virt, prdt_phys));
        self.dma_bufs = Some(dma_bufs);

        ulib::sys_debug_log(bm_base as u64, 0x1DE_0001); // "ide: DMA enabled"
    }

    /// Read sectors using Bus Master DMA. Returns true on success.
    fn read_sectors_dma(&self, lba: u64, count: u32, buf: &mut [u8]) -> bool {
        let bm = self.bm_base;
        if bm == 0 { return false; }

        let (prdt_virt, prdt_phys) = match self.prdt {
            Some(p) => p,
            None => return false,
        };
        let dma_bufs = match &self.dma_bufs {
            Some(b) => b,
            None => return false,
        };

        let total_bytes = count as usize * 512;
        let n_pages = total_bytes.div_ceil(4096);
        if n_pages > MAX_DMA_PAGES { return false; }

        // Build PRDT entries
        let prdt = prdt_virt as *mut [u32; 2];
        for i in 0..n_pages {
            let bytes_this_page = if i == n_pages - 1 {
                total_bytes - i * 4096
            } else {
                4096
            };
            let phys = dma_bufs[i].1 as u32;
            let byte_count = if bytes_this_page == 0x10000 { 0u16 } else { bytes_this_page as u16 };
            let eot: u32 = if i == n_pages - 1 { 0x8000_0000 } else { 0 };
            unsafe {
                let entry = prdt.add(i);
                (*entry)[0] = phys;
                (*entry)[1] = (byte_count as u32) | eot;
            }
        }

        // Stop any prior transfer
        outb(bm + BM_CMD, 0);
        // Clear status bits
        outb(bm + BM_STATUS, BM_STAT_IRQ | BM_STAT_ERROR);
        // Write PRDT physical address
        outd(bm + BM_PRDT, prdt_phys as u32);

        // Issue ATA READ DMA command
        outb(DRIVE_HEAD, 0xE0 | (((lba >> 24) & 0x0F) as u8));
        io_delay();
        if try_wait_not_busy().is_none() { return false; }

        outb(SECTOR_COUNT, count as u8);
        outb(LBA_LO,  (lba        & 0xFF) as u8);
        outb(LBA_MID, ((lba >> 8) & 0xFF) as u8);
        outb(LBA_HI,  ((lba >> 16) & 0xFF) as u8);

        outb(STATUS_CMD, CMD_READ_DMA);

        // Start Bus Master: bit 0 = Start, bit 3 = direction (1 = read from device)
        outb(bm + BM_CMD, 0x09);

        // Spin-poll BM Status for completion
        let mut ok = false;
        for _ in 0..POLL_TIMEOUT {
            let status = inb(bm + BM_STATUS);
            if status & BM_STAT_ERROR != 0 { break; }
            if status & BM_STAT_ACTIVE == 0 {
                ok = true;
                break;
            }
            core::hint::spin_loop();
        }

        // Stop Bus Master
        outb(bm + BM_CMD, 0);
        outb(bm + BM_STATUS, BM_STAT_IRQ | BM_STAT_ERROR);

        if !ok { return false; }

        // Copy from DMA buffers to caller's buffer
        let mut copied = 0usize;
        for i in 0..n_pages {
            let src = dma_bufs[i].0;
            let chunk = (total_bytes - copied).min(4096);
            unsafe {
                core::ptr::copy_nonoverlapping(src, buf.as_mut_ptr().add(copied), chunk);
            }
            copied += chunk;
        }

        true
    }

    /// Read `count` sectors. Uses DMA when available, falls back to PIO.
    pub fn read_sectors(&self, lba: u64, count: u32, buf: &mut [u8]) -> bool {
        if buf.len() < (count as usize) * 512 { return false; }

        if self.bm_base != 0 {
            return self.read_sectors_dma(lba, count, buf);
        }

        self.read_sectors_pio(lba, count, buf)
    }

    fn read_sectors_pio(&self, lba: u64, count: u32, buf: &mut [u8]) -> bool {
        outb(DRIVE_HEAD, 0xE0 | (((lba >> 24) & 0x0F) as u8));
        io_delay();
        if try_wait_not_busy().is_none() { return false; }

        outb(SECTOR_COUNT, count as u8);
        outb(LBA_LO,  (lba        & 0xFF) as u8);
        outb(LBA_MID, ((lba >> 8) & 0xFF) as u8);
        outb(LBA_HI,  ((lba >> 16) & 0xFF) as u8);
        outb(STATUS_CMD, CMD_READ_SECTORS);

        for sector in 0..(count as usize) {
            let s = match try_wait_not_busy() {
                Some(s) => s,
                None => return false,
            };
            if s & DRQ == 0 { return false; }
            if s & ERR != 0 { return false; }

            let offset = sector * 512;
            let chunk = &mut buf[offset..offset + 512];
            for i in 0..256 {
                let word = inw(DATA);
                chunk[i * 2]     = (word & 0xFF) as u8;
                chunk[i * 2 + 1] = (word >> 8)   as u8;
            }
        }
        true
    }

    /// Write `count` sectors. Returns true on success.
    pub fn write_sectors(&self, lba: u64, count: u32, buf: &[u8]) -> bool {
        if buf.len() < (count as usize) * 512 { return false; }

        outb(DRIVE_HEAD, 0xE0 | (((lba >> 24) & 0x0F) as u8));
        io_delay();
        if try_wait_not_busy().is_none() { return false; }

        outb(SECTOR_COUNT, count as u8);
        outb(LBA_LO,  (lba        & 0xFF) as u8);
        outb(LBA_MID, ((lba >> 8) & 0xFF) as u8);
        outb(LBA_HI,  ((lba >> 16) & 0xFF) as u8);
        outb(STATUS_CMD, CMD_WRITE_SECTORS);

        for sector in 0..(count as usize) {
            let s = match try_wait_not_busy() {
                Some(s) => s,
                None => return false,
            };
            if s & DRQ == 0 { return false; }
            if s & ERR != 0 { return false; }

            let offset = sector * 512;
            let chunk = &buf[offset..offset + 512];
            for i in 0..256 {
                let lo = chunk[i * 2]     as u16;
                let hi = chunk[i * 2 + 1] as u16;
                outw(DATA, lo | (hi << 8));
            }
        }

        // Flush write cache
        outb(STATUS_CMD, CMD_CACHE_FLUSH);
        if try_wait_not_busy().is_none() { return false; }
        true
    }
}
