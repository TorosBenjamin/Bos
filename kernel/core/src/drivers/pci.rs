//! PCI configuration space access via legacy I/O mechanism (ports 0xCF8 / 0xCFC).
//!
//! All accesses are serialized by `PCI_LOCK` since the address/data port pair
//! is a shared global resource.

use spin::Mutex;
use x86_64::instructions::port::Port;
use x86_64::PhysAddr;

const CONFIG_ADDRESS: u16 = 0x0CF8;
const CONFIG_DATA: u16 = 0x0CFC;

static PCI_LOCK: Mutex<()> = Mutex::new(());

// ---------- internal raw accessors (caller must hold PCI_LOCK) ----------

unsafe fn raw_read_dword(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    let mut addr_port: Port<u32> = Port::new(CONFIG_ADDRESS);
    let mut data_port: Port<u32> = Port::new(CONFIG_DATA);
    unsafe {
        addr_port.write(config_address(bus, device, function, offset));
        data_port.read()
    }
}

unsafe fn raw_write_dword(bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    let mut addr_port: Port<u32> = Port::new(CONFIG_ADDRESS);
    let mut data_port: Port<u32> = Port::new(CONFIG_DATA);
    unsafe {
        addr_port.write(config_address(bus, device, function, offset));
        data_port.write(value);
    }
}

/// Build a CONFIG_ADDRESS value.
///   bit 31       = enable
///   bits 23:16   = bus
///   bits 15:11   = device (slot)
///   bits 10:8    = function
///   bits  7:2    = register (dword-aligned offset)
///   bits  1:0    = 00
#[inline]
fn config_address(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    (1u32 << 31)
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | ((offset as u32) & 0xFC)
}

/// Read 1, 2, or 4 bytes from PCI configuration space.
///
/// Returns `Some(value)` zero-extended to u32, or `None` on invalid arguments.
pub fn config_read(bus: u8, device: u8, function: u8, offset: u8, width: u8) -> Option<u32> {
    if device > 31 || function > 7 {
        return None;
    }
    match width {
        1 => {}
        2 => {
            if offset & 1 != 0 {
                return None;
            }
        }
        4 => {
            if offset & 3 != 0 {
                return None;
            }
        }
        _ => return None,
    }

    let _guard = PCI_LOCK.lock();
    let addr = config_address(bus, device, function, offset);
    let byte_offset = (offset & 3) as u32;

    unsafe {
        let mut addr_port: Port<u32> = Port::new(CONFIG_ADDRESS);
        let mut data_port: Port<u32> = Port::new(CONFIG_DATA);

        addr_port.write(addr);
        let dword = data_port.read();

        let value = match width {
            1 => (dword >> (byte_offset * 8)) & 0xFF,
            2 => (dword >> (byte_offset * 8)) & 0xFFFF,
            4 => dword,
            _ => unreachable!(),
        };
        Some(value)
    }
}

/// Read a PCI memory-mapped I/O BAR and return `(phys_base, size_in_bytes)`.
///
/// Returns `None` for I/O-port BARs, absent (all-zero) BARs, or invalid arguments.
/// Holds `PCI_LOCK` for the entire write-probe-restore sequence so the operation is atomic.
pub fn read_bar(bus: u8, device: u8, function: u8, bar_index: u8) -> Option<(PhysAddr, u64)> {
    if device > 31 || function > 7 || bar_index > 5 {
        return None;
    }
    let offset = 0x10u8 + bar_index * 4;

    let _guard = PCI_LOCK.lock();

    let (bar0, bar1) = unsafe {
        let b0 = raw_read_dword(bus, device, function, offset);
        let b1 = if (b0 >> 1) & 3 == 2 && bar_index < 5 {
            raw_read_dword(bus, device, function, offset + 4)
        } else {
            0
        };
        (b0, b1)
    };

    // Bit 0 set → I/O-port BAR, not MMIO.
    if bar0 & 1 != 0 {
        return None;
    }

    let is_64bit = (bar0 >> 1) & 3 == 2;
    if is_64bit && bar_index >= 5 {
        return None; // 64-bit BAR would need bar_index+1 which doesn't exist
    }

    let base: u64 = if is_64bit {
        (bar0 as u64 & 0xFFFF_FFF0) | ((bar1 as u64) << 32)
    } else {
        bar0 as u64 & 0xFFFF_FFF0
    };

    // Size probe: write all-1s to BAR(s), read back, then restore original values.
    let size: u64 = unsafe {
        raw_write_dword(bus, device, function, offset, 0xFFFF_FFFF);
        let sz_low = raw_read_dword(bus, device, function, offset) & 0xFFFF_FFF0;

        let sz_high = if is_64bit {
            raw_write_dword(bus, device, function, offset + 4, 0xFFFF_FFFF);
            let sh = raw_read_dword(bus, device, function, offset + 4);
            raw_write_dword(bus, device, function, offset + 4, bar1); // restore high
            sh
        } else {
            0
        };
        raw_write_dword(bus, device, function, offset, bar0); // restore low

        // For 32-bit BARs, compute size in 32-bit space to avoid sign-extending the NOT.
        // e.g. sz_low=0xFFFE0000 (128 KB BAR):
        //   !(0xFFFE0000u32) = 0x0001FFFF  →  + 1 = 0x00020000 = 128 KB  ✓
        // If we did !(0xFFFE0000u64) = 0xFFFF_FFFF_0001_FFFF  →  + 1 = 0xFFFF_FFFF_0002_0000  ✗
        if is_64bit {
            let combined = (sz_low as u64) | ((sz_high as u64) << 32);
            if combined == 0 {
                return None;
            }
            (!combined).wrapping_add(1)
        } else {
            if sz_low == 0 {
                return None;
            }
            (!(sz_low as u32) as u64).wrapping_add(1)
        }
    };

    if base == 0 || size == 0 {
        return None;
    }

    Some((PhysAddr::new(base), size))
}

/// Write 1, 2, or 4 bytes to PCI configuration space.
///
/// Sub-dword writes use read-modify-write. Returns `true` on success.
pub fn config_write(bus: u8, device: u8, function: u8, offset: u8, width: u8, value: u32) -> bool {
    if device > 31 || function > 7 {
        return false;
    }
    match width {
        1 => {}
        2 => {
            if offset & 1 != 0 {
                return false;
            }
        }
        4 => {
            if offset & 3 != 0 {
                return false;
            }
        }
        _ => return false,
    }

    let _guard = PCI_LOCK.lock();
    let addr = config_address(bus, device, function, offset);
    let byte_offset = (offset & 3) as u32;

    unsafe {
        let mut addr_port: Port<u32> = Port::new(CONFIG_ADDRESS);
        let mut data_port: Port<u32> = Port::new(CONFIG_DATA);

        match width {
            4 => {
                addr_port.write(addr);
                data_port.write(value);
            }
            1 | 2 => {
                addr_port.write(addr);
                let mut dword = data_port.read();

                let shift = byte_offset * 8;
                let mask: u32 = match width {
                    1 => 0xFF << shift,
                    2 => 0xFFFF << shift,
                    _ => unreachable!(),
                };
                dword = (dword & !mask) | ((value << shift) & mask);

                addr_port.write(addr);
                data_port.write(dword);
            }
            _ => unreachable!(),
        }
    }
    true
}
