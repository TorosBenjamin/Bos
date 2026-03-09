use crate::descriptors::{RxDesc, TxDesc};
use crate::regs::*;

/// Number of RX and TX descriptors. Must be a multiple of 8 per the spec.
const N: usize = 32;

/// Maximum Ethernet frame payload (excluding 4-byte FCS stripped by SECRC).
pub const MAX_FRAME: usize = 1514;

// Each DMA buffer is one 4 KiB page; 2048 bytes of usable space per buffer.
const BUF_SIZE: usize = 2048;

pub struct E1000 {
    bar0: *mut u8,
    pub mac: [u8; 6],

    rx_ring: *mut RxDesc,
    rx_bufs_virt: [*mut u8; N],
    rx_bufs_phys: [u64; N],
    /// Index of the next descriptor to check for a received packet.
    rx_next: usize,

    tx_ring: *mut TxDesc,
    tx_bufs_virt: [*mut u8; N],
    tx_bufs_phys: [u64; N],
    /// Index of the next descriptor to use for the next transmit.
    tx_next: usize,
    /// Whether each TX slot has been submitted and not yet cleaned.
    tx_submitted: [bool; N],
}

// SAFETY: the driver owns all its DMA pages exclusively and runs single-threaded.
unsafe impl Send for E1000 {}

impl E1000 {
    /// Probe, initialise, and return an E1000 driver.
    ///
    /// Returns `None` if no supported device is found on PCI bus 0 or if any
    /// DMA allocation fails.
    pub fn init() -> Option<Self> {
        let (bus, dev) = find_e1000()?;

        // Enable PCI Bus Master so the NIC can DMA.
        let cmd = ulib::pci_config_read(bus, dev, 0, 0x04, 2).unwrap_or(0);
        ulib::pci_config_write(bus, dev, 0, 0x04, 2, cmd | (1 << 2));

        let bar0 = ulib::sys_map_pci_bar(bus, dev, 0, 0);
        if bar0.is_null() {
            return None;
        }

        // Software reset.
        write_reg(bar0, CTRL, read_reg(bar0, CTRL) | CTRL_RST);
        while read_reg(bar0, CTRL) & CTRL_RST != 0 {}

        // Mask all interrupts; clear any pending.
        write_reg(bar0, IMC, 0xFFFF_FFFF);
        let _ = read_reg(bar0, ICR);

        // Bring the link up.
        write_reg(bar0, CTRL, read_reg(bar0, CTRL) | CTRL_SLU);

        // Read MAC from receive address registers (loaded from EEPROM at reset).
        let ral = read_reg(bar0, RAL0);
        let rah = read_reg(bar0, RAH0);
        let mac = [
            (ral & 0xFF) as u8,
            ((ral >> 8) & 0xFF) as u8,
            ((ral >> 16) & 0xFF) as u8,
            ((ral >> 24) & 0xFF) as u8,
            (rah & 0xFF) as u8,
            ((rah >> 8) & 0xFF) as u8,
        ];

        // Zero the multicast table.
        for i in 0..128u32 {
            write_reg(bar0, MTA_BASE + i * 4, 0);
        }

        // Allocate RX descriptor ring (one 4 KiB page).
        let mut rx_ring_phys: u64 = 0;
        let rx_ring_virt = ulib::sys_alloc_dma(&mut rx_ring_phys);
        if rx_ring_virt.is_null() {
            return None;
        }
        let rx_ring = rx_ring_virt as *mut RxDesc;

        // Allocate N RX packet buffers (one page each, 2 KiB used).
        let mut rx_bufs_virt = [core::ptr::null_mut::<u8>(); N];
        let mut rx_bufs_phys = [0u64; N];
        for i in 0..N {
            let virt = ulib::sys_alloc_dma(&mut rx_bufs_phys[i]);
            if virt.is_null() {
                return None;
            }
            rx_bufs_virt[i] = virt;
        }

        // Initialise RX descriptors: point each at its buffer; clear status.
        for i in 0..N {
            unsafe {
                let desc = &mut *rx_ring.add(i);
                desc.buf_addr = rx_bufs_phys[i];
                desc.length   = 0;
                desc.checksum = 0;
                desc.status   = 0;
                desc.errors   = 0;
                desc.special  = 0;
            }
        }

        // Program RX ring registers.
        write_reg(bar0, RDBAL, (rx_ring_phys & 0xFFFF_FFFF) as u32);
        write_reg(bar0, RDBAH, (rx_ring_phys >> 32) as u32);
        write_reg(bar0, RDLEN, (N * core::mem::size_of::<RxDesc>()) as u32);
        write_reg(bar0, RDH, 0);
        write_reg(bar0, RDT, (N - 1) as u32); // give all descriptors to hardware

        // Enable receiver: EN | BAM | BSIZE=00(2048) | SECRC.
        write_reg(bar0, RCTL, RCTL_EN | RCTL_BAM | RCTL_SECRC);

        // Allocate TX descriptor ring.
        let mut tx_ring_phys: u64 = 0;
        let tx_ring_virt = ulib::sys_alloc_dma(&mut tx_ring_phys);
        if tx_ring_virt.is_null() {
            return None;
        }
        let tx_ring = tx_ring_virt as *mut TxDesc;

        // Allocate N TX packet buffers.
        let mut tx_bufs_virt = [core::ptr::null_mut::<u8>(); N];
        let mut tx_bufs_phys = [0u64; N];
        for i in 0..N {
            let virt = ulib::sys_alloc_dma(&mut tx_bufs_phys[i]);
            if virt.is_null() {
                return None;
            }
            tx_bufs_virt[i] = virt;
        }

        // Zero all TX descriptors.
        unsafe {
            core::ptr::write_bytes(tx_ring_virt, 0, N * core::mem::size_of::<TxDesc>());
        }

        // Program TX ring registers.
        write_reg(bar0, TDBAL, (tx_ring_phys & 0xFFFF_FFFF) as u32);
        write_reg(bar0, TDBAH, (tx_ring_phys >> 32) as u32);
        write_reg(bar0, TDLEN, (N * core::mem::size_of::<TxDesc>()) as u32);
        write_reg(bar0, TDH, 0);
        write_reg(bar0, TDT, 0);

        // Enable transmitter.
        write_reg(bar0, TCTL, TCTL_VAL);
        write_reg(bar0, TIPG, TIPG_VAL);

        Some(E1000 {
            bar0,
            mac,
            rx_ring,
            rx_bufs_virt,
            rx_bufs_phys,
            rx_next: 0,
            tx_ring,
            tx_bufs_virt,
            tx_bufs_phys,
            tx_next: 0,
            tx_submitted: [false; N],
        })
    }

    /// Copy one received packet into `out` and re-arm the descriptor.
    ///
    /// Returns the number of bytes written to `out`, or `None` if no packet
    /// is ready. Silently truncates frames larger than `out.len()`.
    pub fn recv(&mut self, out: &mut [u8]) -> Option<usize> {
        let desc = unsafe { &mut *self.rx_ring.add(self.rx_next) };
        if desc.status & DESC_DD == 0 {
            return None; // hardware hasn't filled this descriptor yet
        }

        let len = (desc.length as usize).min(out.len());
        unsafe {
            core::ptr::copy_nonoverlapping(self.rx_bufs_virt[self.rx_next], out.as_mut_ptr(), len);
        }

        // Clear the status byte and give the descriptor back to hardware.
        desc.status = 0;
        let rdt = self.rx_next;
        self.rx_next = (self.rx_next + 1) % N;
        write_reg(self.bar0, RDT, rdt as u32);

        Some(len)
    }

    /// Transmit `data` as a single Ethernet frame.
    ///
    /// Waits (spins) if the current TX slot is still in-flight from a previous
    /// send. Returns `false` if `data` is empty or larger than `BUF_SIZE`.
    pub fn send(&mut self, data: &[u8]) -> bool {
        if data.is_empty() || data.len() > BUF_SIZE {
            return false;
        }

        let idx = self.tx_next;

        // If we've already used this slot, wait for hardware to mark it done.
        if self.tx_submitted[idx] {
            loop {
                let desc = unsafe { &*self.tx_ring.add(idx) };
                if desc.status & DESC_DD != 0 {
                    break;
                }
            }
        }

        // Copy packet into the DMA buffer.
        unsafe {
            core::ptr::copy_nonoverlapping(data.as_ptr(), self.tx_bufs_virt[idx], data.len());
        }

        // Fill the descriptor.
        unsafe {
            let desc = &mut *self.tx_ring.add(idx);
            desc.buf_addr = self.tx_bufs_phys[idx];
            desc.length   = data.len() as u16;
            desc.cso      = 0;
            desc.cmd      = TX_CMD_EOP | TX_CMD_IFCS | TX_CMD_RS;
            desc.status   = 0;
            desc.css      = 0;
            desc.special  = 0;
        }

        self.tx_submitted[idx] = true;
        self.tx_next = (self.tx_next + 1) % N;

        // Kick the NIC by writing the new tail.
        write_reg(self.bar0, TDT, self.tx_next as u32);
        true
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn find_e1000() -> Option<(u8, u8)> {
    for dev in 0u8..32 {
        let Some(vendor) = ulib::pci_config_read(0, dev, 0, 0x00, 2) else { continue };
        if vendor == 0xFFFF || vendor == 0 {
            continue;
        }
        let Some(device_id) = ulib::pci_config_read(0, dev, 0, 0x02, 2) else { continue };
        if vendor != 0x8086 {
            continue;
        }
        match device_id {
            0x100E | // 82540EM (QEMU default)
            0x100F | // 82545EM
            0x1004 | // 82544GC
            0x10D3   // 82574L (e1000e)
            => return Some((0, dev)),
            _ => {}
        }
    }
    None
}

#[inline]
fn read_reg(bar0: *mut u8, offset: u32) -> u32 {
    unsafe { core::ptr::read_volatile((bar0 as usize + offset as usize) as *const u32) }
}

#[inline]
fn write_reg(bar0: *mut u8, offset: u32, val: u32) {
    unsafe { core::ptr::write_volatile((bar0 as usize + offset as usize) as *mut u32, val) }
}
