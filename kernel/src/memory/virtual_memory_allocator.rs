use core::num::NonZero;

use ez_paging::{ManagedL4PageTable, Page, PageSize};
use nodit::{
    InclusiveInterval, Interval, NoditSet,
    interval::{ii, iu},
};
use x86_64::{PhysAddr, VirtAddr};
use crate::HIGHER_HALF_START;
use crate::memory::hhdm_offset::hhdm_offset;

#[derive(Debug)]
pub struct VirtualMemoryAllocator {
    #[allow(unused)]
    pub(super) set: NoditSet<u64, Interval<u64>>,
    #[allow(unused)]
    pub(super) l4: ManagedL4PageTable,
}

impl VirtualMemoryAllocator {
    /// Returns the start page of the allocated range of pages.
    /// Pages are guaranteed not to be mapped.
    pub fn allocate_contiguous_pages(
        &mut self,
        page_size: PageSize,
        n_pages: NonZero<u64>,
    ) -> Option<Page> {
        let interval = self
            .set
            .gaps_trimmed(&iu(HIGHER_HALF_START))
            .find_map(|gap| {
                let aligned_start = gap.start().next_multiple_of(page_size.byte_len_u64());
                let interval = ii(
                    aligned_start,
                    aligned_start + (n_pages.get() * page_size.byte_len_u64() - 1),
                );
                if gap.contains_interval(&interval) {
                    Some(interval)
                } else {
                    None
                }
            })?;
        self.set
            .insert_merge_touching(interval)
            .expect("no overlap");
        Some(Page::new(VirtAddr::new(*interval.start()), page_size).expect("should be aligned"))
    }

    pub fn l4_mut(&mut self) -> &mut ManagedL4PageTable {
        &mut self.l4
    }
}

pub trait OffsetMappedVirtAddr {
    fn offset_mapped(self) -> PhysAddr;
}

impl OffsetMappedVirtAddr for VirtAddr {
    fn offset_mapped(self) -> PhysAddr {
        PhysAddr::new(self.as_u64() - u64::from(hhdm_offset()))
    }
}