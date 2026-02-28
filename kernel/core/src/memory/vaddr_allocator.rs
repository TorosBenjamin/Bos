use core::num::NonZero;

use crate::memory::hhdm_offset::hhdm_offset;
use nodit::{
    InclusiveInterval, Interval, NoditSet,
    interval::{ii, iu},
};
use x86_64::{PhysAddr, VirtAddr};
use x86_64::structures::paging::{OffsetPageTable, Page, PageSize, PageTable, PhysFrame, Size4KiB};
use crate::consts::{HIGHER_HALF_START, USER_MAX, USER_MIN};

#[derive(Debug)]
pub struct VirtualMemoryAllocator {
    pub(super) set: NoditSet<u64, Interval<u64>>,
    pub(super) l4_phys_frame: PhysFrame<Size4KiB>,
}

impl VirtualMemoryAllocator {
    /// Returns the start page of the allocated range of pages.
    fn allocate_contiguous_pages_in_range(
        &mut self,
        range: Interval<u64>,
        n_pages: NonZero<u64>,
    ) -> Option<Page<Size4KiB>> {
        let page_bytes = Size4KiB::SIZE; // 4096
        let total_bytes = n_pages.get() * page_bytes;

        let interval = self
            .set
            .gaps_trimmed(&range)
            .find_map(|gap| {
                let aligned_start = gap.start().next_multiple_of(page_bytes);
                let end = aligned_start + total_bytes - 1;
                let interval = ii(aligned_start, end);

                gap.contains_interval(&interval).then_some(interval)
            })?;

        self.set
            .insert_merge_touching(interval)
            .expect("no overlap");

        // Returns x86_64::structures::paging::Page
        Some(Page::containing_address(VirtAddr::new(*interval.start())))
    }

    pub fn allocate_kernel_contiguous_pages(&mut self, n_pages: NonZero<u64>) -> Option<Page<Size4KiB>> {
        self.allocate_contiguous_pages_in_range(iu(HIGHER_HALF_START), n_pages)
    }

    pub fn allocate_user_contiguous_pages(&mut self, n_pages: NonZero<u64>) -> Option<Page<Size4KiB>> {
        self.allocate_contiguous_pages_in_range(ii(USER_MIN, USER_MAX), n_pages)
    }

    /// Release a contiguous range of kernel virtual pages back to the allocator.
    pub fn free_kernel_pages(&mut self, start: Page<Size4KiB>, n_pages: NonZero<u64>) {
        let start_addr = start.start_address().as_u64();
        let end_addr = start_addr + n_pages.get() * Size4KiB::SIZE - 1;
        let _ = self.set.cut(&nodit::interval::ii(start_addr, end_addr));
    }

    /// Replaces l4_mut. Returns a standard x86_64 Mapper.
    pub unsafe fn mapper(&mut self) -> OffsetPageTable<'static> {
        let offset = VirtAddr::new(hhdm_offset().as_u64());
        let l4_virt = offset + self.l4_phys_frame.start_address().as_u64();
        unsafe {
            let l4_table = &mut *l4_virt.as_mut_ptr::<PageTable>();
            OffsetPageTable::new(l4_table, offset)
        }
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
