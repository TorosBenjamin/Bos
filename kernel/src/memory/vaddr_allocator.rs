use core::num::NonZero;

use crate::memory::hhdm_offset::hhdm_offset;
use ez_paging::{ManagedL4PageTable, Page, PageSize};
use nodit::{
    InclusiveInterval, Interval, NoditSet,
    interval::{ii, iu},
};
use x86_64::{PhysAddr, VirtAddr};
use crate::consts::{HIGHER_HALF_START, USER_MAX, USER_MIN};

#[derive(Debug)]
pub struct VirtualMemoryAllocator {
    /// Allocated vaddrs
    #[allow(unused)]
    pub(super) set: NoditSet<u64, Interval<u64>>,
    #[allow(unused)]
    pub(super) l4: ManagedL4PageTable,
}

impl VirtualMemoryAllocator {
    /// Returns the start page of the allocated range of pages.
    /// Pages are guaranteed not to be mapped.
    fn allocate_contiguous_pages_in_range(
        &mut self,
        range: Interval<u64>,
        page_size: PageSize,
        n_pages: NonZero<u64>,
    ) -> Option<Page> {
        let page_bytes = page_size.byte_len_u64();
        let total_bytes = n_pages.get() * page_bytes;

        let interval = self
            .set
            .gaps_trimmed(&range) // limit to range
            .find_map(|gap| {
                let aligned_start = gap.start().next_multiple_of(page_bytes);

                let end = aligned_start + total_bytes - 1;
                let interval = ii(aligned_start, end);

                gap.contains_interval(&interval).then_some(interval)
            })?;

        self.set
            .insert_merge_touching(interval)
            .expect("no overlap");

        Page::new(VirtAddr::new(*interval.start()), page_size).ok()
    }

    /// Allocates available vaddr from the kernel's range
    pub fn allocate_kernel_contiguous_pages(
        &mut self,
        page_size: PageSize,
        n_pages: NonZero<u64>,
    ) -> Option<Page> {
        self.allocate_contiguous_pages_in_range(iu(HIGHER_HALF_START), page_size, n_pages)
    }

    /// Allocates available vaddr from the user space range
    pub fn allocate_user_contiguous_pages(
        &mut self,
        page_size: PageSize,
        n_pages: NonZero<u64>,
    ) -> Option<Page> {
        self.allocate_contiguous_pages_in_range(ii(USER_MIN, USER_MAX), page_size, n_pages)
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
