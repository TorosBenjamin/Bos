use acpi::platform::interrupt::{InterruptSourceOverride, Polarity, TriggerMode};
use acpi::platform::InterruptModel;
use acpi::AcpiTables;
use alloc::boxed::Box;
use core::num::NonZero;
use crate::memory::MEMORY;
use spin::Once;
use x86_64::PhysAddr;
use x86_64::structures::paging::{Mapper, PageTableFlags, PhysFrame, Size4KiB};

struct IoApicInfo {
    /// Virtual address of the IOAPIC MMIO registers.
    base: *mut u32,
    /// GSI base for this IOAPIC.
    gsi_base: u32,
}

// Safety: IOAPIC MMIO is accessed only with proper synchronization via the
// spin::Once guard (init runs once) and individual register accesses are
// inherently atomic 32-bit MMIO reads/writes.
unsafe impl Send for IoApicInfo {}
unsafe impl Sync for IoApicInfo {}

struct IoApicState {
    info: IoApicInfo,
    interrupt_source_overrides: &'static [InterruptSourceOverride],
}

static IOAPIC: Once<IoApicState> = Once::new();

// Leaked box for 'static lifetime of overrides
static ISO_STORAGE: Once<&'static [InterruptSourceOverride]> = Once::new();

/// IOREGSEL register offset (index register)
const IOREGSEL: usize = 0x00;
/// IOWIN register offset (data register)
const IOWIN: usize = 0x10;

/// Redirection table entry registers start at index 0x10.
/// Each entry is two 32-bit registers (low + high).
const IOREDTBL_BASE: u8 = 0x10;

fn read_register(base: *mut u32, index: u8) -> u32 {
    unsafe {
        let reg_sel = base.byte_add(IOREGSEL);
        let reg_win = base.byte_add(IOWIN);
        core::ptr::write_volatile(reg_sel, index as u32);
        core::ptr::read_volatile(reg_win)
    }
}

fn write_register(base: *mut u32, index: u8, value: u32) {
    unsafe {
        let reg_sel = base.byte_add(IOREGSEL);
        let reg_win = base.byte_add(IOWIN);
        core::ptr::write_volatile(reg_sel, index as u32);
        core::ptr::write_volatile(reg_win, value);
    }
}

/// Read the maximum number of redirection entries from IOAPICVER register.
fn max_redirection_entries(base: *mut u32) -> u8 {
    let ver = read_register(base, 0x01);
    ((ver >> 16) & 0xFF) as u8
}

/// Mask all IOAPIC pins by setting the mask bit in each redirection entry.
fn mask_all(base: *mut u32) {
    let max_entries = max_redirection_entries(base);
    for i in 0..=max_entries {
        let reg_low = IOREDTBL_BASE + i * 2;
        let low = read_register(base, reg_low);
        // Set bit 16 (mask bit)
        write_register(base, reg_low, low | (1 << 16));
    }
}

/// Map the IOAPIC MMIO page with writable + uncacheable flags.
///
/// The IOAPIC sits at a device MMIO address (typically 0xFEC00000) which is
/// not part of normal RAM and therefore not covered by the Limine HHDM.
/// We must explicitly map it, just like the local APIC MMIO page.
fn map_ioapic_mmio(phys_addr: u32) -> *mut u32 {
    let memory = MEMORY.get().unwrap();
    let mut physical_memory = memory.physical_memory.lock();
    let mut virtual_memory = memory.virtual_memory.lock();

    // 1. Create the physical frame
    let frame: PhysFrame<Size4KiB> = PhysFrame::containing_address(PhysAddr::new(phys_addr as u64));

    // 2. Allocate 1 virtual page from the kernel range
    let page = virtual_memory
        .allocate_kernel_contiguous_pages(NonZero::new(1).unwrap())
        .expect("Failed to allocate virtual page for IOAPIC");

    // 3. Define MMIO flags (Uncacheable)
    let flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::NO_CACHE
        | PageTableFlags::WRITE_THROUGH;

    // 4. Get mapper and allocator
    let mut mapper = unsafe { virtual_memory.mapper() };
    let mut frame_allocator = physical_memory.get_kernel_frame_allocator();

    // 5. Perform the mapping
    unsafe {
        mapper.map_to(page, frame, flags, &mut frame_allocator)
            .expect("Failed to map IOAPIC MMIO")
            .flush();
    }

    // 6. Return the virtual pointer
    page.start_address().as_mut_ptr()
}

/// Initialize the IOAPIC subsystem from ACPI tables.
///
/// This explicitly maps the IOAPIC MMIO registers and masks all IRQ lines.
pub fn init(acpi_tables: &AcpiTables<impl acpi::Handler>) {
    let apic_model = match InterruptModel::new(acpi_tables).unwrap().0 {
        InterruptModel::Apic(apic) => apic,
        _ => panic!("No APIC interrupt model found"),
    };

    // We only support a single IOAPIC for now
    let io_apic = apic_model.io_apics.first().expect("No IOAPIC found in ACPI tables");

    // Map IOAPIC MMIO page explicitly (not via HHDM — device MMIO isn't in RAM)
    let virt_addr = map_ioapic_mmio(io_apic.address);

    // Store interrupt source overrides with 'static lifetime
    let overrides: alloc::vec::Vec<InterruptSourceOverride> =
        apic_model.interrupt_source_overrides.into_iter().collect();
    let overrides_boxed = overrides.into_boxed_slice();
    let overrides_static: &'static [InterruptSourceOverride] = Box::leak(overrides_boxed);
    ISO_STORAGE.call_once(|| overrides_static);

    IOAPIC.call_once(|| {
        let info = IoApicInfo {
            base: virt_addr,
            gsi_base: io_apic.global_system_interrupt_base,
        };

        // Mask all pins initially
        mask_all(info.base);

        log::info!(
            "IOAPIC initialized at phys={:#x}, virt={:#p}, GSI base={}",
            io_apic.address,
            virt_addr,
            io_apic.global_system_interrupt_base,
        );

        // Disable legacy PIC (mask all IRQs on 8259)
        if apic_model.also_has_legacy_pics {
            disable_legacy_pic();
        }

        IoApicState {
            info,
            interrupt_source_overrides: *ISO_STORAGE.get().unwrap(),
        }
    });
}

/// Disable the legacy 8259 PIC by masking all IRQs.
fn disable_legacy_pic() {
    use x86::io::outb;
    unsafe {
        outb(0x21, 0xFF); // PIC1 data
        outb(0xA1, 0xFF); // PIC2 data
    }
    log::info!("Legacy 8259 PIC disabled");
}

/// Route ISA IRQ1 (keyboard) to the specified APIC vector on the given destination APIC.
///
/// Handles ACPI interrupt source overrides (ISA IRQ1 may be remapped to a different GSI).
pub fn enable_keyboard_irq(vector: u8, dest_apic_id: u32) {
    let state = IOAPIC.get().expect("IOAPIC not initialized");

    // Check for interrupt source override for ISA IRQ 1
    let (gsi, polarity, trigger_mode) = state
        .interrupt_source_overrides
        .iter()
        .find(|iso| iso.isa_source == 1)
        .map(|iso| (iso.global_system_interrupt, iso.polarity, iso.trigger_mode))
        .unwrap_or((1, Polarity::SameAsBus, TriggerMode::SameAsBus));

    // Calculate the IOAPIC pin from the GSI
    let pin = (gsi - state.info.gsi_base) as u8;

    // Build the redirection table entry
    let mut entry_low: u32 = vector as u32; // bits 0-7: vector

    // Delivery mode: fixed (000)
    // Destination mode: physical (bit 11 = 0)

    // Polarity (bit 13)
    match polarity {
        Polarity::ActiveLow => entry_low |= 1 << 13,
        _ => {}
    }

    // Trigger mode (bit 15)
    match trigger_mode {
        TriggerMode::Level => entry_low |= 1 << 15,
        _ => {}
    }

    // Unmask (bit 16 = 0, already clear)

    let entry_high: u32 = (dest_apic_id & 0xFF) << 24; // bits 56-63: destination

    let reg_low = IOREDTBL_BASE + pin * 2;
    let reg_high = reg_low + 1;

    write_register(state.info.base, reg_high, entry_high);
    write_register(state.info.base, reg_low, entry_low);

    log::info!(
        "IOAPIC: Keyboard IRQ1 -> GSI {} -> pin {} -> vector {:#x}, dest APIC {}",
        gsi,
        pin,
        vector,
        dest_apic_id,
    );
}
