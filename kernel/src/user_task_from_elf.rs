use crate::limine_requests::{MODULE_REQUEST, USER_LAND_PATH};
use crate::memory::MEMORY;
use crate::memory::cpu_local_data::get_local;
use crate::memory::physical_memory::{MemoryType, OffsetMappedPhysAddr, OffsetMappedPhysFrame};
use crate::memory::vaddr_allocator::OffsetMappedVirtAddr;
use crate::task::task::Task;
use bitflags::bitflags;
use core::num::NonZero;
use core::ops::Range;
use core::ptr::{NonNull, slice_from_raw_parts_mut};
use elf::ElfBytes;
use elf::endian::AnyEndian;
use elf::segment::ProgramHeader;
use ez_paging::{ConfigurableFlags, Frame, Page, PageSize};
use nodit::interval::ie;
use nodit::{Interval, NoditSet};
use num_enum::{IntoPrimitive, TryFromPrimitive};
use x86_64::registers::model_specific::PatMemoryType;
use x86_64::{PhysAddr, VirtAddr};
use crate::consts::LOWER_HALF_END;

/// Create a user-mode task from the first Limine module matching USER_LAND_PATH.
///
/// This parses the ELF, creates a new address space, maps ELF segments and a
/// user stack, then returns a `Task` ready to be scheduled.
pub fn create_user_task_from_elf() -> Task {
    let module = MODULE_REQUEST
        .get_response()
        .unwrap()
        .modules()
        .iter()
        .find(|module| module.path() == USER_LAND_PATH)
        .unwrap();

    let ptr = NonNull::new(slice_from_raw_parts_mut(
        module.addr(),
        module.size() as usize,
    ))
    .unwrap();

    // Safety: Limine provides valid pointer and len
    let elf_bytes = unsafe { ptr.as_ref() };

    let elf = ElfBytes::<AnyEndian>::minimal_parse(elf_bytes).expect("Failed to parse ELF");

    // Track user-space virtual address allocations
    let mut user_vaddr_set: NoditSet<u64, Interval<u64>> = NoditSet::default();

    // Create new address space for user mode
    let memory = MEMORY.get().unwrap();
    let mut physical_memory = memory.physical_memory.lock();
    let mut user_l4 = memory.virtual_memory.lock().l4_mut().new_user(
        physical_memory
            .get_user_mode_frame_allocator()
            .allocate_frame_4kib()
            .unwrap(),
    );

    // Capture CR3 physical address from the allocated L4 frame
    let cr3 = user_l4.frame().start_address().as_u64();

    // Remove the module from physical memory map
    let page_size = PageSize::_4KiB;
    let module_physical_interval = {
        let start = VirtAddr::from_ptr(module.addr()).offset_mapped().as_u64();
        ie(
            start,
            (start + module.size()).next_multiple_of(page_size.byte_len_u64()),
        )
    };
    let _ = physical_memory.map_mut().cut(&module_physical_interval);

    // Map ELF segments
    let mut range_to_zero: Option<Range<PhysAddr>> = None;
    for segment in elf.segments().unwrap() {
        if ElfSegmentType::try_from(segment.p_type) != Ok(ElfSegmentType::Load) {
            continue;
        }

        // Make sure the segment is only referencing file memory contained within the ELF
        assert!(segment.p_offset + segment.p_filesz <= module.size());

        let start_page = Page::new(
            VirtAddr::new(segment.p_vaddr).align_down(page_size.byte_len_u64()),
            page_size,
        )
        .unwrap();
        let file_pages_len = if segment.p_filesz > 0 {
            (segment.p_vaddr + segment.p_filesz).div_ceil(page_size.byte_len_u64())
                - segment.p_vaddr / page_size.byte_len_u64()
        } else {
            0
        };
        let start_frame = Frame::new(
            (VirtAddr::from_ptr(module.addr()).offset_mapped() + segment.p_offset)
                .align_down(page_size.byte_len_u64()),
            page_size,
        )
        .unwrap();

        // Map the virtual memory to the ELF's frames
        let flags = ElfSegmentFlags::from(segment);
        let mut frame_allocator = physical_memory.get_user_mode_frame_allocator();
        let flags = ConfigurableFlags {
            pat_memory_type: PatMemoryType::WriteBack,
            writable: flags.contains(ElfSegmentFlags::WRITABLE),
            executable: flags.contains(ElfSegmentFlags::EXECUTABLE),
        };
        for i in 0..file_pages_len {
            let page = start_page.offset(i).unwrap();
            let frame = start_frame.offset(i).unwrap();
            unsafe { user_l4.map_page(page, frame, flags, &mut frame_allocator) }.unwrap();
        }

        // Mark the ELF's frames as used by user mode
        if file_pages_len > 0 {
            let interval = {
                let start = start_frame.start_addr().as_u64();
                ie(start, start + file_pages_len * page_size.byte_len_u64())
            };
            let _ = physical_memory.map_mut().cut(&interval);
            physical_memory
                .map_mut()
                .insert_merge_touching_if_values_equal(interval, MemoryType::UsedByUserMode)
                .unwrap();
        }

        let mut total_pages = file_pages_len;

        if segment.p_memsz > segment.p_filesz {
            if segment.p_filesz > 0 {
                // We need to zero any remaining bytes from that frame
                assert_eq!(
                    range_to_zero, None,
                    "there can only be up to 1 segment with p_memsz > p_filesz"
                );
                range_to_zero = Some({
                    let start = VirtAddr::from_ptr(module.addr()).offset_mapped()
                        + segment.p_offset
                        + segment.p_filesz;
                    start..start.align_up(page_size.byte_len_u64())
                });
            }

            // We need to allocate, zero, and map additional frames
            let extra_pages_len = (segment.p_vaddr + segment.p_memsz)
                .div_ceil(page_size.byte_len_u64())
                - (segment.p_vaddr + segment.p_filesz).div_ceil(page_size.byte_len_u64());
            let start_page = start_page.offset(file_pages_len).unwrap();
            for i in 0..extra_pages_len {
                let page = start_page.offset(i).unwrap();
                let frame = physical_memory
                    .allocate_frame_with_type(page_size, MemoryType::UsedByUserMode)
                    .unwrap();
                let frame_ptr =
                    NonNull::new(frame.offset_mapped().start_addr().as_mut_ptr::<u8>()).unwrap();
                // Safety: we own the frame
                unsafe { frame_ptr.write_bytes(0, page_size.byte_len()) };
                let mut frame_allocator = physical_memory.get_user_mode_frame_allocator();
                unsafe { user_l4.map_page(page, frame, flags, &mut frame_allocator) }.unwrap();
            }
            total_pages += extra_pages_len;
        }

        // Track the mapped virtual address range
        if total_pages > 0 {
            let seg_start = start_page.start_addr().as_u64();
            let seg_end = seg_start + total_pages * page_size.byte_len_u64();
            user_vaddr_set
                .insert_merge_touching(ie(seg_start, seg_end))
                .expect("ELF segment vaddr overlap");
        }
    }

    // Map all non-referenced frames in the ELF as usable
    loop {
        let gap = physical_memory
            .map_mut()
            .gaps_trimmed(&module_physical_interval)
            .next();
        if let Some(gap) = gap {
            physical_memory
                .map_mut()
                .insert_merge_touching_if_values_equal(gap, MemoryType::Usable)
                .unwrap();
        } else {
            break;
        }
    }

    // Parse entry point before dropping 'elf'
    let entry_point = NonZero::new(elf.ehdr.e_entry).expect("ELF does not define an entry point");

    // Zero the range we need to zero, if needed
    if let Some(range_to_zero) = range_to_zero {
        let count = (range_to_zero.end - range_to_zero.start) as usize;
        let ptr = NonNull::new(range_to_zero.start.offset_mapped().as_mut_ptr::<u8>()).unwrap();
        // Safety: we now have exclusive access to the ELF bytes
        unsafe { ptr.write_bytes(0, count) };
    }

    // Zero the unused bytes of the last frame of the ELF module, if it's used
    {
        let start = module.addr().addr() + module.size() as usize;
        let ptr = NonNull::new(start as *mut u8).unwrap();
        let end = start.next_multiple_of(page_size.byte_len());
        let count = end - start;
        // Safety: we have exclusive access to this memory
        unsafe { ptr.write_bytes(0, count) };
    }

    // Allocate a user stack at the top of the canonical lower half.
    // LOWER_HALF_END is 0x7FFFFFFFFFFF (inclusive). Align down to page boundary
    // for the stack top, then subtract one page so RSP starts within mapped pages.
    let rsp = (LOWER_HALF_END + 1) - 0x1000;
    {
        let stack_size: u64 = 64 * 0x400;
        let page_size = PageSize::_4KiB;
        let pages_len = stack_size.div_ceil(page_size.byte_len_u64());
        let stack_start_vaddr = rsp - pages_len * page_size.byte_len_u64();
        let start_page = Page::new(
            VirtAddr::new(stack_start_vaddr),
            page_size,
        )
        .unwrap();
        for i in 0..pages_len {
            let page = start_page.offset(i).unwrap();
            let frame = physical_memory
                .allocate_frame_with_type(page_size, MemoryType::UsedByUserMode)
                .unwrap();
            let flags = ConfigurableFlags {
                pat_memory_type: PatMemoryType::WriteBack,
                writable: true,
                executable: false,
            };
            let mut frame_allocator = physical_memory.get_user_mode_frame_allocator();
            unsafe { user_l4.map_page(page, frame, flags, &mut frame_allocator) }.unwrap()
        }
        // Track the stack virtual address range
        user_vaddr_set
            .insert_merge_touching(ie(stack_start_vaddr, stack_start_vaddr + pages_len * page_size.byte_len_u64()))
            .expect("user stack vaddr overlap");
    };

    // Release memory lock
    drop(physical_memory);

    // Get user segment selectors from the GDT
    let local = get_local();
    let gdt = local.gdt.get().unwrap();
    let user_cs = gdt.user_code_selector().0;
    let user_ss = gdt.user_data_selector().0;

    Task::new_user(entry_point.get(), rsp, user_l4, cr3, user_cs, user_ss, user_vaddr_set, 0)
}

#[derive(Debug)]
pub enum SpawnError {
    InvalidElf,
    OutOfMemory,
}

/// Create a user-mode task from raw ELF bytes (e.g. from user memory during a Spawn syscall).
///
/// Unlike `create_user_task_from_elf`, this allocates fresh physical frames and copies
/// ELF segment data into them, giving the child fully independent memory.
pub fn create_user_task_from_elf_bytes(elf_bytes: &[u8], child_arg: u64) -> Result<Task, SpawnError> {
    let elf = ElfBytes::<AnyEndian>::minimal_parse(elf_bytes)
        .map_err(|_| SpawnError::InvalidElf)?;

    let mut user_vaddr_set: NoditSet<u64, Interval<u64>> = NoditSet::default();

    let memory = MEMORY.get().unwrap();
    let mut physical_memory = memory.physical_memory.lock();
    let mut user_l4 = memory.virtual_memory.lock().l4_mut().new_user(
        physical_memory
            .get_user_mode_frame_allocator()
            .allocate_frame_4kib()
            .ok_or(SpawnError::OutOfMemory)?,
    );

    let cr3 = user_l4.frame().start_address().as_u64();

    let page_size = PageSize::_4KiB;

    // Map ELF LOAD segments
    for segment in elf.segments().ok_or(SpawnError::InvalidElf)? {
        if ElfSegmentType::try_from(segment.p_type) != Ok(ElfSegmentType::Load) {
            continue;
        }

        // Validate the segment references data within the ELF bytes
        if segment.p_offset + segment.p_filesz > elf_bytes.len() as u64 {
            return Err(SpawnError::InvalidElf);
        }

        let start_page = Page::new(
            VirtAddr::new(segment.p_vaddr).align_down(page_size.byte_len_u64()),
            page_size,
        )
        .map_err(|_| SpawnError::InvalidElf)?;

        let file_pages_len = if segment.p_filesz > 0 {
            (segment.p_vaddr + segment.p_filesz).div_ceil(page_size.byte_len_u64())
                - segment.p_vaddr / page_size.byte_len_u64()
        } else {
            0
        };

        let flags = ElfSegmentFlags::from(segment);
        let configurable_flags = ConfigurableFlags {
            pat_memory_type: PatMemoryType::WriteBack,
            writable: flags.contains(ElfSegmentFlags::WRITABLE),
            executable: flags.contains(ElfSegmentFlags::EXECUTABLE),
        };

        // Allocate fresh frames and copy file data for pages that contain file content
        for i in 0..file_pages_len {
            let page = start_page.offset(i).unwrap();
            let frame = physical_memory
                .allocate_frame_with_type(page_size, MemoryType::UsedByUserMode)
                .ok_or(SpawnError::OutOfMemory)?;

            // Zero the frame first, then copy the relevant bytes
            let frame_virt = frame.offset_mapped().start_addr().as_mut_ptr::<u8>();
            let frame_virt = NonNull::new(frame_virt).unwrap();
            unsafe { frame_virt.write_bytes(0, page_size.byte_len()) };

            // Calculate which bytes from the ELF to copy into this frame
            let page_vaddr = page.start_addr().as_u64();
            let seg_file_start = segment.p_vaddr; // vaddr where file data starts
            let seg_file_end = segment.p_vaddr + segment.p_filesz;

            let copy_start_vaddr = page_vaddr.max(seg_file_start);
            let copy_end_vaddr = (page_vaddr + page_size.byte_len_u64()).min(seg_file_end);

            if copy_start_vaddr < copy_end_vaddr {
                let offset_in_page = (copy_start_vaddr - page_vaddr) as usize;
                let offset_in_file = (segment.p_offset + (copy_start_vaddr - segment.p_vaddr)) as usize;
                let count = (copy_end_vaddr - copy_start_vaddr) as usize;

                unsafe {
                    core::ptr::copy_nonoverlapping(
                        elf_bytes.as_ptr().add(offset_in_file),
                        frame_virt.as_ptr().add(offset_in_page),
                        count,
                    );
                }
            }

            let mut frame_allocator = physical_memory.get_user_mode_frame_allocator();
            unsafe { user_l4.map_page(page, frame, configurable_flags, &mut frame_allocator) }
                .map_err(|_| SpawnError::OutOfMemory)?;
        }

        let mut total_pages = file_pages_len;

        // Handle BSS (p_memsz > p_filesz): allocate zeroed extra pages
        if segment.p_memsz > segment.p_filesz {
            let extra_pages_len = (segment.p_vaddr + segment.p_memsz)
                .div_ceil(page_size.byte_len_u64())
                - (segment.p_vaddr + segment.p_filesz).div_ceil(page_size.byte_len_u64());
            let bss_start_page = start_page.offset(file_pages_len).unwrap();
            for i in 0..extra_pages_len {
                let page = bss_start_page.offset(i).unwrap();
                let frame = physical_memory
                    .allocate_frame_with_type(page_size, MemoryType::UsedByUserMode)
                    .ok_or(SpawnError::OutOfMemory)?;
                let frame_ptr =
                    NonNull::new(frame.offset_mapped().start_addr().as_mut_ptr::<u8>()).unwrap();
                unsafe { frame_ptr.write_bytes(0, page_size.byte_len()) };
                let mut frame_allocator = physical_memory.get_user_mode_frame_allocator();
                unsafe { user_l4.map_page(page, frame, configurable_flags, &mut frame_allocator) }
                    .map_err(|_| SpawnError::OutOfMemory)?;
            }
            total_pages += extra_pages_len;
        }

        // Track the mapped virtual address range
        if total_pages > 0 {
            let seg_start = start_page.start_addr().as_u64();
            let seg_end = seg_start + total_pages * page_size.byte_len_u64();
            user_vaddr_set
                .insert_merge_touching(ie(seg_start, seg_end))
                .expect("ELF segment vaddr overlap");
        }
    }

    // Parse entry point
    let entry_point = NonZero::new(elf.ehdr.e_entry).ok_or(SpawnError::InvalidElf)?;

    // Allocate a user stack at the top of the canonical lower half
    let rsp = (LOWER_HALF_END + 1) - 0x1000;
    {
        let stack_size: u64 = 64 * 0x400;
        let pages_len = stack_size.div_ceil(page_size.byte_len_u64());
        let stack_start_vaddr = rsp - pages_len * page_size.byte_len_u64();
        let start_page = Page::new(VirtAddr::new(stack_start_vaddr), page_size)
            .map_err(|_| SpawnError::OutOfMemory)?;
        for i in 0..pages_len {
            let page = start_page.offset(i).unwrap();
            let frame = physical_memory
                .allocate_frame_with_type(page_size, MemoryType::UsedByUserMode)
                .ok_or(SpawnError::OutOfMemory)?;
            let flags = ConfigurableFlags {
                pat_memory_type: PatMemoryType::WriteBack,
                writable: true,
                executable: false,
            };
            let mut frame_allocator = physical_memory.get_user_mode_frame_allocator();
            unsafe { user_l4.map_page(page, frame, flags, &mut frame_allocator) }
                .map_err(|_| SpawnError::OutOfMemory)?;
        }
        user_vaddr_set
            .insert_merge_touching(ie(stack_start_vaddr, stack_start_vaddr + pages_len * page_size.byte_len_u64()))
            .expect("user stack vaddr overlap");
    }

    drop(physical_memory);

    let local = get_local();
    let gdt = local.gdt.get().unwrap();
    let user_cs = gdt.user_code_selector().0;
    let user_ss = gdt.user_data_selector().0;

    Ok(Task::new_user(entry_point.get(), rsp, user_l4, cr3, user_cs, user_ss, user_vaddr_set, child_arg))
}

bitflags! {
    #[derive(Debug, Clone, Copy)]
    pub struct ElfSegmentFlags: u32 {
        const EXECUTABLE = 1 << 0;
        const WRITABLE = 1 << 1;
        const READABLE = 1 << 2;

        // The source may set any bits
        const _ = !0;
    }
}

impl From<ProgramHeader> for ElfSegmentFlags {
    fn from(value: ProgramHeader) -> Self {
        Self::from_bits_retain(value.p_flags)
    }
}

#[non_exhaustive]
#[repr(u32)]
#[derive(Debug, TryFromPrimitive, IntoPrimitive, PartialEq, Eq)]
enum ElfSegmentType {
    Load = 0x1,
}
