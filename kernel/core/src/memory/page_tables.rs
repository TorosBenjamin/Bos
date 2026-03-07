use crate::memory::hhdm_offset::hhdm_offset;
use crate::memory::physical_memory::{PhysicalMemory};
use crate::memory::vaddr_allocator::VirtualMemoryAllocator;
use limine::memory_map::EntryType;
use limine::response::MemoryMapResponse;
use nodit::NoditSet;
use nodit::interval::iu;
use x86_64::{PhysAddr, VirtAddr};
use x86_64::registers::control::{Cr3, Cr3Flags};
use x86_64::structures::paging::{Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame, Size2MiB, Size4KiB, Translate};

/// Creates new page tables
pub fn create_page_tables(
    memory_map: &'static MemoryMapResponse,
    physical_memory: &mut PhysicalMemory,
) -> (PhysFrame, Cr3Flags, VirtualMemoryAllocator) {
    let hhdm_offset = hhdm_offset();
    let mut frame_allocator = physical_memory.get_kernel_frame_allocator();

    // Allocate and initialize the new L4 Frame
    let l4_frame: PhysFrame<Size4KiB> = frame_allocator.allocate_frame_4kib().expect("No frames for L4");
    let l4_virt = VirtAddr::new(hhdm_offset.as_u64() + l4_frame.start_address().as_u64());

    // Zero out the new page table
    unsafe {
        let ptr = l4_virt.as_mut_ptr::<PageTable>();
        ptr.write(PageTable::new());
    }

    let mut mapper = unsafe {
        OffsetPageTable::new(&mut *l4_virt.as_mut_ptr::<PageTable>(), VirtAddr::new(hhdm_offset.as_u64()))
    };

    // Map the physical memory regions (HHDM mapping)
    // Uses 2 MiB huge pages for aligned middle regions, 4 KiB for head/tail.
    const SIZE_2MIB: u64 = 2 * 1024 * 1024;
    const MASK_2MIB: u64 = SIZE_2MIB - 1;

    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;

    for entry in memory_map.entries() {
        if !matches!(entry.entry_type,
            EntryType::USABLE |
            EntryType::BOOTLOADER_RECLAIMABLE |
            EntryType::EXECUTABLE_AND_MODULES |
            EntryType::FRAMEBUFFER
        ) { continue; }

        let start = entry.base;
        let end   = entry.base + entry.length;
        let aligned_start = (start + MASK_2MIB) & !MASK_2MIB;
        let aligned_end   = end & !MASK_2MIB;

        // Head: 4 KiB pages before first 2 MiB boundary
        for phys_addr in (start..aligned_start.min(end)).step_by(4096) {
            let frame: PhysFrame<Size4KiB> = PhysFrame::containing_address(PhysAddr::new(phys_addr));
            let page:  Page<Size4KiB>      = Page::containing_address(VirtAddr::new(hhdm_offset.as_u64() + phys_addr));
            unsafe { mapper.map_to(page, frame, flags, &mut frame_allocator).expect("HHDM 4K").flush(); }
        }

        // Middle: 2 MiB huge pages
        if aligned_start < aligned_end {
            for phys_addr in (aligned_start..aligned_end).step_by(SIZE_2MIB as usize) {
                let frame: PhysFrame<Size2MiB> = PhysFrame::containing_address(PhysAddr::new(phys_addr));
                let page:  Page<Size2MiB>      = Page::containing_address(VirtAddr::new(hhdm_offset.as_u64() + phys_addr));
                unsafe { mapper.map_to(page, frame, flags, &mut frame_allocator).expect("HHDM 2M").flush(); }
            }
        }

        // Tail: 4 KiB pages after last 2 MiB boundary
        // Condition: boundary is within the region AND there is content past it.
        // (aligned_end >= start guards against double-mapping when the head already
        //  covers the whole region because aligned_start > end.)
        if aligned_end >= start && aligned_end < end {
            for phys_addr in (aligned_end..end).step_by(4096) {
                let frame: PhysFrame<Size4KiB> = PhysFrame::containing_address(PhysAddr::new(phys_addr));
                let page:  Page<Size4KiB>      = Page::containing_address(VirtAddr::new(hhdm_offset.as_u64() + phys_addr));
                unsafe { mapper.map_to(page, frame, flags, &mut frame_allocator).expect("HHDM 4K").flush(); }
            }
        }
    }

    // 4. Clone the Kernel/Limine mappings (Index 511)
    let (current_l4_frame, cr3_flags) = Cr3::read();
    let current_l4_virt = VirtAddr::new(hhdm_offset.as_u64() + current_l4_frame.start_address().as_u64());

    unsafe {
        let current_table = &*current_l4_virt.as_ptr::<PageTable>();
        let new_table = &mut *l4_virt.as_mut_ptr::<PageTable>();
        new_table[511] = current_table[511].clone();
    }

    (
        l4_frame,
        cr3_flags,
        VirtualMemoryAllocator {
            set: {
                let mut set = NoditSet::default();
                for entry in memory_map.entries() {
                    if matches!(entry.entry_type,
                        EntryType::USABLE |
                        EntryType::BOOTLOADER_RECLAIMABLE |
                        EntryType::EXECUTABLE_AND_MODULES |
                        EntryType::FRAMEBUFFER
                    ) {
                        let start = hhdm_offset.as_u64() + (entry.base / 4096) * 4096;
                        let end = hhdm_offset.as_u64() + ((entry.base + entry.length - 1) / 4096) * 4096 + 4095;
                        set.insert_merge_touching_or_overlapping((start..=end).into());
                    }
                }
                set.insert_merge_touching(iu(0xFFFFFF8000000000)).unwrap();
                set
            },
            l4_phys_frame: l4_frame,
        },
    )
}

pub fn get_kernel_vaddr_from_user_vaddr(
    user_l4_phys_frame: PhysFrame,
    user_vaddr: VirtAddr,
) -> Option<VirtAddr> {
    let offset = VirtAddr::new(hhdm_offset().as_u64());

    // Recreate a mapper for the user's page table
    let l4_virt = offset + user_l4_phys_frame.start_address().as_u64();
    let mapper = unsafe {
        OffsetPageTable::new(&mut *l4_virt.as_mut_ptr::<PageTable>(), offset)
    };

    // Use the translate_addr method from the Translate trait
    let phys_addr = mapper.translate_addr(user_vaddr)?;

    // Return the HHDM version of that physical address
    Some(offset + phys_addr.as_u64())
}