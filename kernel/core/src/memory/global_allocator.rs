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

/// A region of physical memory claimed by the global allocator.
pub struct ClaimedRegion {
    pub start: PhysAddr,
    pub size: u64,
}

/// Finds unused physical memory for the global allocator and initializes the global allocator.
/// Claims USABLE entries from the memory map until GLOBAL_ALLOCATOR_SIZE bytes are collected,
/// or all USABLE entries are exhausted.
///
/// Returns the list of claimed physical regions so the physical memory tracker can mark them.
///
/// # Safety
/// This function must be called exactly once, and no page tables should be modified before calling this function.
pub unsafe fn init(memory_map: &'static MemoryMapResponse) -> [Option<ClaimedRegion>; MAX_CLAIMED_REGIONS] {
    let mut talc = GLOBAL_ALLOCATOR.lock();
    let mut claimed = [const { None }; MAX_CLAIMED_REGIONS];
    let mut claimed_count = 0;
    let mut total_claimed: u64 = 0;

    // Sort entries by size (largest first) to minimize number of regions
    // We can't allocate a Vec yet, so use a fixed-size array of indices
    let entries = memory_map.entries();
    let mut indices = [0u16; MAX_CLAIMED_REGIONS];
    let mut num_usable = 0;
    for (i, entry) in entries.iter().enumerate() {
        if entry.entry_type == EntryType::USABLE && entry.length > 0 {
            if num_usable < MAX_CLAIMED_REGIONS {
                indices[num_usable] = i as u16;
                num_usable += 1;
            }
        }
    }
    // Simple insertion sort by length descending
    for i in 1..num_usable {
        let mut j = i;
        while j > 0 && entries[indices[j] as usize].length > entries[indices[j - 1] as usize].length {
            indices.swap(j, j - 1);
            j -= 1;
        }
    }

    for idx in &indices[..num_usable] {
        if total_claimed >= GLOBAL_ALLOCATOR_SIZE {
            break;
        }

        let entry = &entries[*idx as usize];
        let take = entry.length.min(GLOBAL_ALLOCATOR_SIZE - total_claimed);

        let phys_start = PhysAddr::new(entry.base);
        let mem = {
            let mut ptr = NonNull::new(slice_from_raw_parts_mut(
                phys_start.offset_mapped().as_mut_ptr::<MaybeUninit<u8>>(),
                take as usize,
            ))
            .unwrap();
            // Safety: Physical memory must be reserved and offset mapped
            unsafe { ptr.as_mut() }
        };
        let span = mem.into();
        // Safety: Span must be from valid memory
        unsafe { talc.claim(span) }.unwrap();

        claimed[claimed_count] = Some(ClaimedRegion {
            start: phys_start,
            size: take,
        });
        claimed_count += 1;
        total_claimed += take;
    }

    assert!(
        total_claimed > 0,
        "No USABLE memory found for global allocator"
    );

    if total_claimed < GLOBAL_ALLOCATOR_SIZE {
        log::warn!(
            "Global allocator: only {:#x} bytes available (wanted {:#x})",
            total_claimed,
            GLOBAL_ALLOCATOR_SIZE,
        );
    }

    claimed
}

const MAX_CLAIMED_REGIONS: usize = 16;
