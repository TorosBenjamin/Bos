use crate::memory::physical_memory::OffsetMappedPhysAddr;
use core::mem::MaybeUninit;
use core::ptr::{NonNull, slice_from_raw_parts_mut};
use limine::memory_map::EntryType;
use limine::response::MemoryMapResponse;
use talc::{ErrOnOom, Talc, Talck};
use x86_64::PhysAddr;

pub const GLOBAL_ALLOCATOR_SIZE: u64 = {
    // 4 MiB
    4 * 0x400 * 0x400
};

#[global_allocator]
pub static GLOBAL_ALLOCATOR: Talck<spin::Mutex<()>, ErrOnOom> = Talck::new({
    // Initially, there is no memory backing `Talc`. We will add memory at run time
    Talc::new(ErrOnOom)
});

/// Finds unused physical memory for the global allocator and initializes the global allocator.
/// Returns the start address of the physical memory used for the global allocator.
///
/// # Safety
/// This function must be called exactly once, and no page tables should be modified before calling this function.
pub unsafe fn init(memory_map: &'static MemoryMapResponse) -> PhysAddr {
    let global_allocator_physical_start = PhysAddr::new(
        memory_map
            .entries()
            .iter()
            .find(|entry| {
                entry.entry_type == EntryType::USABLE && entry.length >= GLOBAL_ALLOCATOR_SIZE
            })
            .unwrap()
            .base,
    );
    let global_allocator_mem = {
        let mut ptr = NonNull::new(slice_from_raw_parts_mut(
            global_allocator_physical_start
                .offset_mapped()
                .as_mut_ptr::<MaybeUninit<u8>>(),
            GLOBAL_ALLOCATOR_SIZE as usize,
        ))
        .unwrap();
        // Safety: Physical memory must be reserved and offset mapped
        unsafe { ptr.as_mut() }
    };
    let mut talc = GLOBAL_ALLOCATOR.lock();
    let span = global_allocator_mem.into();
    // Safety: Span must be from valid memory
    unsafe { talc.claim(span) }.unwrap();

    global_allocator_physical_start
}
