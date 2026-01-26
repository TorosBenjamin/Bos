use core::cell::UnsafeCell;
use crate::memory::MEMORY;
use crate::memory::cpu_local_data::get_local;
use acpi::AcpiTables;
use acpi::platform::InterruptModel;
use core::num::NonZero;
use ez_paging::{ConfigurableFlags, Frame, PageSize};
use force_send_sync::SendSync;
use raw_cpuid::CpuId;
use spin::Once;
use x2apic::lapic::LocalApicBuilder;
use x86_64::registers::model_specific::{Msr, PatMemoryType};
use x86_64::{PhysAddr, VirtAddr};
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
            // Local apic is always exactly 4KiB, aligned to 4KiB
            let page_size = PageSize::_4KiB;
            let frame = Frame::new(PhysAddr::new(apic.local_apic_address), page_size).unwrap();

            let memory = MEMORY.get().unwrap();
            let mut physical_memory = memory.physical_memory.lock();
            let mut frame_allocator = physical_memory.get_kernel_frame_allocator();
            let mut virtual_memory = memory.virtual_memory.lock();
            let page = virtual_memory
                .allocate_kernel_contiguous_pages(page_size, NonZero::new(1).unwrap())
                .unwrap();
            let flags = ConfigurableFlags {
                writable: true,
                executable: false,
                // We use strong uncacheable memory type, because reads and writes have side effects
                pat_memory_type: PatMemoryType::StrongUncacheable,
            };
            // Safety: We map to the correct page for the Local APIC
            unsafe {
                virtual_memory
                    .l4_mut()
                    .map_page(page, frame, flags, &mut frame_allocator)
            }
            .unwrap();
            LocalApicAccess::Mmio(page.start_addr())
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
