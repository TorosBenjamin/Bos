use crate::memory::global_allocator;
use crate::memory::hhdm_offset::hhdm_offset;
use limine::memory_map::EntryType;
use limine::response::MemoryMapResponse;
use nodit::{Interval, NoditMap};
use x86_64::structures::paging::{FrameAllocator, Page, PageSize, PhysFrame, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};
use crate::exceptions::FreeError;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum KernelMemoryUsageType {
    PageTables,
    GlobalAllocatorHeap,
    Stack,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum MemoryType {
    Usable,
    UsedByLimine,
    UsedByKernel(KernelMemoryUsageType),
    UsedByUserMode,
}

#[derive(Debug)]
pub struct PhysicalMemory {
    /// A map of used physical memory
    map: NoditMap<u64, Interval<u64>, MemoryType>,
}

/// Track used physical memory
impl PhysicalMemory {
    pub(super) fn new(
        memory_map: &'static MemoryMapResponse,
        global_allocator_start: PhysAddr,
    ) -> Self {
        Self {
            map: {
                let mut map = NoditMap::default();
                // Start from the state when Limine booted
                for entry in memory_map.entries() {
                    let should_insert = match entry.entry_type {
                        EntryType::USABLE => Some(MemoryType::Usable),
                        EntryType::BOOTLOADER_RECLAIMABLE => Some(MemoryType::UsedByLimine),
                        _ => {
                            // The entry might overlap, so let's not add it
                            None
                        }
                    };
                    if let Some(memory_type) = should_insert {
                        map.insert_merge_touching_if_values_equal(
                            (entry.base..entry.base + entry.length).into(),
                            memory_type,
                        )
                        .unwrap();
                    }
                }
                // Track the memory used for the global allocator
                let interval = Interval::from(
                    global_allocator_start.as_u64()
                        ..global_allocator_start.as_u64() + global_allocator::GLOBAL_ALLOCATOR_SIZE,
                );
                let _ = map.cut(&interval);
                map.insert_merge_touching_if_values_equal(
                    interval,
                    MemoryType::UsedByKernel(KernelMemoryUsageType::GlobalAllocatorHeap),
                )
                .unwrap();
                map
            },
        }
    }

    pub fn get_kernel_frame_allocator(&mut self) -> PhysicalMemoryFrameAllocator<'_> {
        PhysicalMemoryFrameAllocator {
            physical_memory: self,
            memory_type: MemoryType::UsedByKernel(KernelMemoryUsageType::PageTables),
        }
    }

    pub fn get_user_mode_frame_allocator(&mut self) -> PhysicalMemoryFrameAllocator<'_> {
        PhysicalMemoryFrameAllocator {
            physical_memory: self,
            memory_type: MemoryType::UsedByUserMode,
        }
    }

    pub fn allocate_frame_with_type(
        &mut self,
        memory_type: MemoryType,
    ) -> Option<PhysFrame<Size4KiB>> {
        let size = Size4KiB::SIZE; // 4096

        let aligned_start = self.map.iter().find_map(|(interval, m_type)| {
            if let MemoryType::Usable = m_type {
                let aligned_start = (*interval.start()).next_multiple_of(size);
                let required_end = aligned_start + size;

                if required_end <= *interval.end() {
                    Some(aligned_start)
                } else {
                    None
                }
            } else {
                None
            }
        })?;

        let range = aligned_start..aligned_start + size;
        let _ = self.map.cut(&Interval::from(range.clone()));
        self.map
            .insert_merge_touching_if_values_equal(range.into(), memory_type)
            .unwrap();

        Some(PhysFrame::containing_address(PhysAddr::new(aligned_start)))
    }

    pub fn free_frame(
        &mut self,
        frame: PhysFrame<Size4KiB>,
        expected: MemoryType,
    ) -> Result<(), FreeError> {
        let start = frame.start_address().as_u64();
        let size = Size4KiB::SIZE;
        let end = start + size - 1;

        // Check if the frame exists in our map and matches the type
        let (_, found_type) = self
            .map
            .iter()
            .find(|(i, _)| {
                *i.start() <= start && *i.end() >= end
            })
            .ok_or(FreeError::FrameNotAllocated)?;

        if *found_type != expected {
            return Err(FreeError::WrongMemoryType {
                expected,
                found: *found_type,
            });
        }

        // Re-mark the range as Usable
        let _ = self.map.cut(&Interval::from(start..start + size));
        self.map
            .insert_merge_touching_if_values_equal(
                Interval::from(start..start + size),
                MemoryType::Usable,
            )
            .unwrap();

        Ok(())
    }

    pub fn is_frame_allocated(&self, frame: PhysFrame<Size4KiB>) -> bool {
        let start = frame.start_address().as_u64();
        let end = start + Size4KiB::SIZE - 1;
        self.map.iter().any(|(i, _)| {
            *i.start() <= start && *i.end() >= end
        })
    }


    pub fn map_mut(&mut self) -> &mut NoditMap<u64, Interval<u64>, MemoryType> {
        &mut self.map
    }
}

pub struct PhysicalMemoryFrameAllocator<'a> {
    physical_memory: &'a mut PhysicalMemory,
    memory_type: MemoryType,
}

impl PhysicalMemoryFrameAllocator<'_> {
    pub fn allocate_frame_4kib(&mut self) -> Option<PhysFrame<Size4KiB>> {
        self.physical_memory
            .allocate_frame_with_type(self.memory_type)
    }
}

// The trait implementation is now a direct passthrough
unsafe impl FrameAllocator<Size4KiB> for PhysicalMemoryFrameAllocator<'_> {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        self.allocate_frame_4kib()
    }
}

pub trait OffsetMappedPhysAddr {
    fn offset_mapped(self) -> VirtAddr;
}
impl OffsetMappedPhysAddr for PhysAddr {
    fn offset_mapped(self) -> VirtAddr {
        VirtAddr::new(self.as_u64() + u64::from(hhdm_offset()))
    }
}
pub trait OffsetMappedPhysFrame {
    fn offset_mapped(self) -> Page<Size4KiB>;
}

impl OffsetMappedPhysFrame for PhysFrame<Size4KiB> {
    fn offset_mapped(self) -> Page<Size4KiB> {
        Page::containing_address(self.start_address().offset_mapped())
    }
}
