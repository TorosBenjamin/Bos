use limine::response::MemoryMapResponse;
use spin::Once;
use x86_64::registers::control::{Cr3, Cr3Flags};
use x86_64::structures::paging::{PhysFrame, Size4KiB};
use crate::memory::page_tables::create_page_tables;
use crate::memory::physical_memory::PhysicalMemory;
use crate::memory::virtual_memory_allocator::VirtualMemoryAllocator;

pub mod cpu_local_data;
pub mod global_allocator;
pub mod physical_memory;
pub mod page_tables;
pub mod hhdm_offset;
pub mod virtual_memory_allocator;
pub mod guarded_stack;

/// Initializes global allocator, creates new page tables, and switches to new page tables.
/// This function must be called before mapping pages or running our kernel's code on APs.
///
/// # Safety
/// This function must be called exactly once, and no page tables should be modified before calling this function.

#[non_exhaustive]
#[derive(Debug)]
pub struct Memory {
    #[allow(unused)]
    pub physical_memory: spin::Mutex<PhysicalMemory>,
    #[allow(unused)]
    pub virtual_memory: spin::Mutex<VirtualMemoryAllocator>,
    pub new_kernel_cr3: PhysFrame<Size4KiB>,
    pub new_kernel_cr3_flags: Cr3Flags,
}

pub static MEMORY: Once<Memory> = Once::new();

/// Initializes global allocator, creates new page tables, and switches to new page tables.
/// This function must be called before mapping pages or running our kernel's code on APs.
///
/// # Safety
/// This function must be called exactly once, and no page tables should be modified before calling this function.
pub unsafe fn init_bsp(memory_map: &'static MemoryMapResponse) {
    let global_allocator_start = unsafe { global_allocator::init(memory_map) };
    let mut physical_memory = PhysicalMemory::new(memory_map, global_allocator_start);
    let (new_kernel_cr3, new_kernel_cr3_flags, virtual_memory) =
        create_page_tables(memory_map, &mut physical_memory);
    // Safety: page tables are ready to be used
    unsafe { Cr3::write(new_kernel_cr3, new_kernel_cr3_flags) };
    MEMORY.call_once(|| Memory {
        physical_memory: spin::Mutex::new(physical_memory),
        virtual_memory: spin::Mutex::new(virtual_memory),
        new_kernel_cr3,
        new_kernel_cr3_flags,
    });
}

/// # Safety
/// Must be called on all APs before modifying page tables
pub unsafe fn init_ap() {
    let memory = MEMORY.get().unwrap();
    // Safety: page tables are ready to be used
    unsafe { Cr3::write(memory.new_kernel_cr3, memory.new_kernel_cr3_flags) };
}