use core::cell::UnsafeCell;
use crate::memory::MEMORY;
use crate::memory::cpu_local_data::get_local;
use acpi::AcpiTables;
use acpi::platform::InterruptModel;
use core::num::NonZero;
use force_send_sync::SendSync;
use raw_cpuid::CpuId;
use spin::Once;
use x2apic::lapic::LocalApicBuilder;
use x86_64::registers::model_specific::Msr;
use x86_64::{PhysAddr, VirtAddr};
use x86_64::structures::paging::{Mapper, PageTableFlags, PhysFrame, Size4KiB};
use crate::interrupt::InterruptVector;

const IA32_X2APIC_SVR: u32 = 0x80F;

pub enum LocalApicAccess {
    RegisterBased,
    /// Pointer to the mapped local Apic
    Mmio(VirtAddr),
}

pub static LOCAL_APIC_ACCESS: Once<LocalApicAccess> = Once::new();

/// Maps the Local APIC memory if needed, and initializes LOCAL_APIC_ACCESS
pub fn init_bsp(acpi_tables: &AcpiTables<impl acpi::Handler>) {
    let apic = match InterruptModel::new(acpi_tables).unwrap().0 {
        InterruptModel::Apic(apic) => apic,
        interrupt_model => panic!("Unknown interrupt model: {:#?}", interrupt_model),
    };
    LOCAL_APIC_ACCESS.call_once(|| {
        if cpu_has_x2apic() {
            log::info!("x2apic support enabled");
            LocalApicAccess::RegisterBased
        } else {
            log::info!("x2apic support disabled");

            // 1. Setup constants and address types
            let memory = MEMORY.get().unwrap();
            let mut physical_memory = memory.physical_memory.lock();
            let mut virtual_memory = memory.virtual_memory.lock();

            let lapic_phys_addr = PhysAddr::new(apic.local_apic_address);
            let frame: PhysFrame<Size4KiB> = PhysFrame::containing_address(lapic_phys_addr);

            // 2. Allocate 1 virtual page
            let page = virtual_memory
                .allocate_kernel_contiguous_pages(NonZero::new(1).unwrap())
                .expect("Failed to allocate virtual page for LAPIC");

            // 3. Define Flags for MMIO
            // We replace PatMemoryType::StrongUncacheable with standard PageTableFlags.
            // For MMIO, we use NO_CACHE (PCD) and WRITE_THROUGH (PWT).
            let flags = PageTableFlags::PRESENT
                | PageTableFlags::WRITABLE
                | PageTableFlags::NO_CACHE
                | PageTableFlags::WRITE_THROUGH;

            // 4. Map the page
            let mut mapper = unsafe { virtual_memory.mapper() };
            let mut frame_allocator = physical_memory.get_kernel_frame_allocator();

            unsafe {
                mapper.map_to(page, frame, flags, &mut frame_allocator)
                    .expect("Failed to map Local APIC MMIO")
                    .flush();
            }

            // 5. Return the virtual address for access
            LocalApicAccess::Mmio(page.start_address())
        }
    });
}

/// This function needs to be called on all CPUs.
/// [`init_bsp`] must be called first.
pub fn init_local_apic() {
    get_local().local_apic.call_once(|| {
        UnsafeCell::new({
            let local_apic = {
                let mut builder = LocalApicBuilder::new();
                // Only `set_xapic_base` if x2APIC is not supported
                if let LocalApicAccess::Mmio(address) = LOCAL_APIC_ACCESS.get().unwrap() {
                    builder.set_xapic_base(address.as_u64());
                }

                builder.spurious_vector(u8::from(InterruptVector::LocalApicSpurious).into());
                builder.error_vector(u8::from(InterruptVector::LocalApicError).into());
                builder.timer_vector(u8::from(InterruptVector::LocalApicTimer).into());

                let mut local_apic = builder.build().unwrap();
                unsafe { local_apic.enable() }
                // The builder arms a Periodic timer (initial=10M, unmasked) by default.
                // Mask it immediately so no stray interrupt fires before lapic_timer::init().
                unsafe { local_apic.disable_timer() }
                local_apic
            };
            unsafe { SendSync::new(local_apic) }
        })
    });
}

fn cpu_has_x2apic() -> bool {
    let cpuid = CpuId::new();

    match cpuid.get_feature_info() {
        Some(info) => info.has_x2apic(),
        None => false,
    }
}


pub fn is_enabled() -> bool {
    let svr = unsafe { Msr::new(IA32_X2APIC_SVR).read() };
    svr & (1 << 8) != 0
}

/// Send a fixed-delivery IPI to the given APIC ID on the given vector.
pub fn send_fixed_ipi(target_apic_id: u32, vector: u8) {
    match LOCAL_APIC_ACCESS.get().unwrap() {
        LocalApicAccess::RegisterBased => {
            // x2APIC: write ICR (MSR 0x830) with destination in bits [63:32] and vector in [7:0]
            let icr = ((target_apic_id as u64) << 32) | vector as u64;
            unsafe { Msr::new(0x830).write(icr) };
        }
        LocalApicAccess::Mmio(base) => {
            // xAPIC MMIO: write ICR_HIGH (base+0x310) first, then ICR_LOW (base+0x300)
            let base = base.as_u64();
            unsafe {
                core::ptr::write_volatile((base + 0x310) as *mut u32, target_apic_id << 24);
                core::ptr::write_volatile((base + 0x300) as *mut u32, vector as u32);
            }
        }
    }
}
