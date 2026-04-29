//! # Sound Server
//!
//! AC'97 audio driver and mixer for Bos OS.
//!
//! Discovers the Intel AC'97 PCI device (vendor=0x8086, device=0x2415),
//! initialises the codec and DMA engine, then serves PCM audio chunks
//! from client tasks over an IPC channel.
//!
//! ## IPC protocol
//! See `kernel_api_types::audio` for message wire format.
//!
//! ## AC'97 register map
//! BAR0 = NAM (Native Audio Mixer) — I/O port base for codec registers.
//! BAR1 = NABM (Native Audio Bus Master) — I/O port base for DMA controller.

#![no_std]
#![no_main]

use kernel_api_types::audio::{AUDIO_MSG_PLAY_CHUNK, AUDIO_MSG_SET_VOLUME, AUDIO_PLAY_CHUNK_LEN};
use ulib::handle;

// ── AC'97 NAM register offsets (relative to BAR0 I/O port base) ──────────────
const NAM_RESET:       u16 = 0x00; // Global reset
const NAM_MASTER_VOL:  u16 = 0x02; // Master volume (R+L, 6-bit attenuation each)
const NAM_PCM_VOL:     u16 = 0x18; // PCM out volume
const NAM_PCM_RATE:    u16 = 0x2C; // PCM front DAC sample rate

// ── AC'97 NABM register offsets (relative to BAR1 I/O port base) ─────────────
// PCM out channel starts at offset 0x10.
const NABM_PCM_BDBAR:  u16 = 0x10; // Buffer Descriptor Base Address Register (u32)
const NABM_PCM_CIV:    u16 = 0x14; // Current Index Value (u8, read-only)
const NABM_PCM_LVI:    u16 = 0x15; // Last Valid Index (u8)
const NABM_PCM_SR:     u16 = 0x16; // Status register (u16) — write 1s to clear
const NABM_PCM_CR:     u16 = 0x1B; // Control register (u8): bit0=RUN, bit1=RESET, bit3=FEIE, bit4=IOCE

// NABM global control
const NABM_GLOB_CNT:   u16 = 0x2C; // Global control register (u32)
const _NABM_GLOB_STS:  u16 = 0x30; // Global status register (u32, reserved for future use)

// CR bits
const CR_RUN:   u8 = 0x01;
const CR_RESET: u8 = 0x02;

// BDL flags
const BDL_FLAG_IOC: u16 = 0x8000; // Interrupt On Completion
const BDL_FLAG_BUP: u16 = 0x4000; // Buffer Underrun Policy (repeat last sample)

// ── BDL geometry ──────────────────────────────────────────────────────────────
const N_BDL:    usize = 32;        // Hardware maximum BDL entries
const N_BUFS:   usize = 16;        // Number of backing DMA pages (must be > N_AHEAD)
const BUF_BYTES: usize = 4096;     // Bytes per DMA page
// At 44100 Hz, stereo, 16-bit: 4096 bytes / 4 bytes-per-sample = 1024 stereo frames ≈ 23 ms
const SAMPLES_PER_BUF: u16 = (BUF_BYTES / 2) as u16; // 16-bit samples per buffer (both channels interleaved)

// ── PCM mix ring ──────────────────────────────────────────────────────────────
// Hold up to 256 KB of queued s16 samples (~1.5 s).
const MIX_RING_SAMPLES: usize = 128 * 1024;

// ── BDL entry layout (8 bytes, LE) ───────────────────────────────────────────
#[repr(C, packed)]
struct BdlEntry {
    phys:     u32,
    nsamples: u16,
    flags:    u16,
}

// ── Driver state ──────────────────────────────────────────────────────────────
struct Ac97 {
    nam:  u16,  // NAM I/O port base
    nabm: u16,  // NABM I/O port base

    /// Virtual addresses of BDL page and audio DMA pages.
    bdl_virt:  *mut BdlEntry,
    buf_virt:  [*mut u8; N_BUFS],

    /// Physical address of BDL page (stored for debugging / future reconfigure).
    _bdl_phys: u32,
    /// Physical addresses of audio DMA pages.
    buf_phys:  [u32; N_BUFS],

    /// Next BDL slot we intend to fill; advances as we stage new audio.
    write_head: usize,

    /// PCM mix ring (interleaved s16le, left then right).
    ring: [i16; MIX_RING_SAMPLES],
    ring_read:  usize,
    ring_write: usize,
    ring_count: usize,
}

impl Ac97 {
    /// Discover and initialise the AC'97 device. Returns None if not found.
    fn init() -> Option<Self> {
        let (bus, dev) = find_ac97()?;

        // Read BAR0 and BAR1 — they are I/O space BARs (bit 0 set).
        let bar0 = ulib::pci_config_read(bus, dev, 0, 0x10, 4)? & !0x3;
        let bar1 = ulib::pci_config_read(bus, dev, 0, 0x14, 4)? & !0x3;
        let nam  = bar0 as u16;
        let nabm = bar1 as u16;

        ulib::sys_debug_log(((bus as u64) << 8) | dev as u64, 0xAC97); // AC97 found: bus|dev
        ulib::sys_debug_log(bar0 as u64, 0xAC98); // NAM I/O base
        ulib::sys_debug_log(bar1 as u64, 0xAC99); // NABM I/O base

        // Enable PCI Bus Master (bit 2) and I/O space (bit 0).
        let cmd = ulib::pci_config_read(bus, dev, 0, 0x04, 2).unwrap_or(0);
        ulib::pci_config_write(bus, dev, 0, 0x04, 2, cmd | 0x05);

        // --- Codec cold reset ---
        // NABM GCR bit 1 is COLD_RESET# (active-low): 0 = asserted, 1 = deasserted.
        // Assert reset, wait, then deassert to wake the codec.
        ulib::outd(nabm + NABM_GLOB_CNT, 0x0000_0000); // assert cold reset
        ulib::sys_sleep_ms(10);
        ulib::outd(nabm + NABM_GLOB_CNT, 0x0000_0002); // deassert cold reset
        ulib::sys_sleep_ms(50);                         // wait for codec ready

        // NAM software reset (write any value to offset 0x00).
        ulib::outw(nam + NAM_RESET, 0);
        ulib::sys_sleep_ms(10);

        // --- Set volumes: 0x0000 = max volume, 0x8080 = mute ---
        ulib::outw(nam + NAM_MASTER_VOL, 0x0000); // 0 dB
        ulib::outw(nam + NAM_PCM_VOL,    0x0808); // slight attenuation to avoid clipping

        // Set sample rate to 44100 Hz (VRA must be supported; QEMU always supports it)
        ulib::outw(nam + NAM_PCM_RATE, 44100);
        ulib::sys_sleep_ms(5);

        // --- Allocate DMA pages ---
        let mut bdl_phys: u64 = 0;
        let bdl_virt = ulib::sys_alloc_dma(&mut bdl_phys);
        if bdl_virt.is_null() {
            return None;
        }
        // Zero the BDL page.
        unsafe { core::ptr::write_bytes(bdl_virt, 0, 4096) };

        let mut buf_virt = [core::ptr::null_mut::<u8>(); N_BUFS];
        let mut buf_phys = [0u32; N_BUFS];
        for i in 0..N_BUFS {
            let mut phys: u64 = 0;
            let virt = ulib::sys_alloc_dma(&mut phys);
            if virt.is_null() {
                return None;
            }
            // Silence.
            unsafe { core::ptr::write_bytes(virt, 0, BUF_BYTES) };
            buf_virt[i] = virt;
            buf_phys[i] = phys as u32;
        }

        let bdl = bdl_virt as *mut BdlEntry;

        // Fill all 32 BDL entries (silence, IOC|BUP on every entry).
        for i in 0..N_BDL {
            unsafe {
                (*bdl.add(i)).phys     = buf_phys[i % N_BUFS];
                (*bdl.add(i)).nsamples = SAMPLES_PER_BUF;
                (*bdl.add(i)).flags    = BDL_FLAG_IOC | BDL_FLAG_BUP;
            }
        }

        // Reset PCM out channel.
        ulib::outb(nabm + NABM_PCM_CR, CR_RESET);
        ulib::sys_sleep_ms(5);
        ulib::outb(nabm + NABM_PCM_CR, 0);
        ulib::sys_sleep_ms(2);

        // Point BDBAR at our BDL.
        ulib::outd(nabm + NABM_PCM_BDBAR, bdl_phys as u32);

        // Clear status register.
        ulib::outw(nabm + NABM_PCM_SR, 0x001E);

        // Set LVI = 31 (all entries valid).
        ulib::outb(nabm + NABM_PCM_LVI, (N_BDL - 1) as u8);

        // Start DMA.
        ulib::outb(nabm + NABM_PCM_CR, CR_RUN);

        Some(Ac97 {
            nam,
            nabm,
            bdl_virt: bdl,
            buf_virt,
            buf_phys,
            _bdl_phys: bdl_phys as u32,
            write_head: 0,
            ring: [0i16; MIX_RING_SAMPLES],
            ring_read: 0,
            ring_write: 0,
            ring_count: 0,
        })
    }

    /// Set master volume (0 = max, 100 = mute equivalent; we map linearly).
    fn set_volume(&mut self, vol: u8) {
        // AC'97 volume: 0 = 0 dB, 63 = -94.5 dB (mute). 6-bit per channel.
        // vol=0 → attenuation=0, vol=100 → attenuation=63.
        let atten = (vol as u16 * 63) / 100;
        let reg = (atten << 8) | atten; // same for L and R
        ulib::outw(self.nam + NAM_MASTER_VOL, reg);
    }

    /// Enqueue s16le PCM samples into the mix ring. Drops samples if ring is full.
    fn enqueue(&mut self, samples: &[i16]) {
        for &s in samples {
            if self.ring_count < MIX_RING_SAMPLES {
                self.ring[self.ring_write] = s;
                self.ring_write = (self.ring_write + 1) % MIX_RING_SAMPLES;
                self.ring_count += 1;
            }
        }
    }

    /// Advance the DMA engine: fill BDL slots ahead of the hardware cursor.
    fn pump(&mut self) {
        let civ = ulib::inb(self.nabm + NABM_PCM_CIV) as usize;

        // We want to keep N_AHEAD = 10 slots queued ahead of CIV.
        const N_AHEAD: usize = 10;
        let target_lvi = (civ + N_AHEAD) % N_BDL;

        // Fill slots from write_head up to (but not including) target_lvi+1.
        loop {
            if self.write_head == (target_lvi + 1) % N_BDL {
                break;
            }
            let slot = self.write_head;
            let buf_idx = slot % N_BUFS;
            let buf = self.buf_virt[buf_idx];

            // Fill this DMA page with samples from the ring (or silence).
            let samples_needed = SAMPLES_PER_BUF as usize;
            let buf_i16 = buf as *mut i16;
            for j in 0..samples_needed {
                let s = if self.ring_count > 0 {
                    let v = self.ring[self.ring_read];
                    self.ring_read = (self.ring_read + 1) % MIX_RING_SAMPLES;
                    self.ring_count -= 1;
                    v
                } else {
                    0i16
                };
                unsafe { buf_i16.add(j).write_volatile(s) };
            }

            // Update the BDL entry to point at the (possibly reused) DMA page.
            unsafe {
                (*self.bdl_virt.add(slot)).phys     = self.buf_phys[buf_idx];
                (*self.bdl_virt.add(slot)).nsamples = SAMPLES_PER_BUF;
                (*self.bdl_virt.add(slot)).flags    = BDL_FLAG_IOC | BDL_FLAG_BUP;
            }

            self.write_head = (self.write_head + 1) % N_BDL;
        }

        // Update LVI so the hardware knows these entries are valid.
        ulib::outb(self.nabm + NABM_PCM_LVI, target_lvi as u8);

        // Clear DCH/CELV/LVBCI/BCIS status bits if set.
        ulib::outw(self.nabm + NABM_PCM_SR, 0x001E);
    }
}

// ── PCI discovery ─────────────────────────────────────────────────────────────

fn find_ac97() -> Option<(u8, u8)> {
    for bus in 0u8..8 {
        for dev in 0u8..32 {
            let Some(vendor) = ulib::pci_config_read(bus, dev, 0, 0x00, 2) else { continue };
            if vendor == 0xFFFF || vendor == 0 {
                continue;
            }
            let Some(device_id) = ulib::pci_config_read(bus, dev, 0, 0x02, 2) else { continue };
            // Log every Intel device so we can see what's on the bus.
            if vendor == 0x8086 {
                ulib::sys_debug_log(((bus as u64) << 24) | ((dev as u64) << 16) | device_id as u64, 0xAC00);
            }
            if vendor == 0x8086 && device_id == 0x2415 {
                return Some((bus, dev));
            }
        }
    }
    None
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point(_arg: u64) -> ! {
    ulib::sys_debug_log(0, 0xAC_0001); // sound_server started
    let mut ac97 = match Ac97::init() {
        Some(d) => {
            ulib::sys_debug_log(0, 0xAC_0002); // AC97 init OK
            ulib::log::write(ulib::log::LogLevel::Info, "sound_server", "AC97 initialised");
            d
        }
        None => {
            ulib::sys_debug_log(0, 0xAC_0003); // AC97 not found
            ulib::log::write(ulib::log::LogLevel::Error, "sound_server", "AC97 not found");
            ulib::sys_exit(1);
        }
    };

    // Create the service channel and register it.
    let (send_fd, recv_fd) = handle::channel(32).unwrap();
    handle::register_service(b"audio", send_fd);

    let mut msg_buf = [0u8; 4096 + 32];

    loop {
        // ── 1. Advance DMA engine ─────────────────────────────────────────────
        ac97.pump();

        // ── 2. Drain incoming IPC messages ────────────────────────────────────
        loop {
            let n = match handle::try_read(recv_fd, &mut msg_buf) {
                Some(n) if n > 0 => n,
                _ => break,
            };

            if n == 0 {
                continue;
            }

            match msg_buf[0] {
                AUDIO_MSG_PLAY_CHUNK if n >= AUDIO_PLAY_CHUNK_LEN => {
                    // [type:u8][buf_id:u64 LE][n_samples:u32 LE][channels:u8][sample_rate:u32 LE]
                    let buf_id: u64 = unsafe {
                        core::ptr::read_unaligned(msg_buf.as_ptr().add(1) as *const u64)
                    };
                    let n_samples: u32 = unsafe {
                        core::ptr::read_unaligned(msg_buf.as_ptr().add(9) as *const u32)
                    };
                    // channels and sample_rate read but not used yet (we assume 44100 stereo)

                    // Map the shared buffer.
                    let ptr = ulib::sys_map_shared_buf(buf_id);
                    if !ptr.is_null() {
                        let samples = unsafe {
                            core::slice::from_raw_parts(ptr as *const i16, n_samples as usize)
                        };
                        ac97.enqueue(samples);
                        ulib::sys_munmap(ptr, (n_samples as u64) * 2);
                    }
                    ulib::sys_destroy_shared_buf(buf_id);
                }

                AUDIO_MSG_SET_VOLUME if n >= 2 => {
                    ac97.set_volume(msg_buf[1]);
                }

                _ => {} // unknown or truncated — ignore
            }
        }

        // ── 3. Sleep ~8 ms (≈ half a DMA buffer at 44100 Hz) ─────────────────
        ulib::sys_wait_for_event(&[recv_fd as u64], 0, 8);
    }
}

#[panic_handler]
fn rust_panic(_info: &core::panic::PanicInfo) -> ! {
    ulib::default_panic(_info)
}
