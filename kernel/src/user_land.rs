use crate::limine_requests::{MODULE_REQUEST, USER_LAND_PATH};
use crate::memory::MEMORY;
use crate::memory::physical_memory::{MemoryType, OffsetMappedPhysAddr, OffsetMappedPhysFrame};
use crate::memory::vaddr_allocator::OffsetMappedVirtAddr;
use crate::{raw_syscall_handler};
use bitflags::bitflags;
use core::arch::asm;
use core::num::NonZero;
use core::ops::Range;
use core::ptr::{NonNull, slice_from_raw_parts_mut};
use core::sync::atomic::{AtomicBool, Ordering};
use elf::ElfBytes;
use elf::endian::AnyEndian;
use elf::segment::ProgramHeader;
use ez_paging::{ConfigurableFlags, Frame, Page, PageSize};
use nodit::interval::ie;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use x86_64::registers::model_specific::PatMemoryType;
use x86_64::registers::rflags::RFlags;
use x86_64::{PhysAddr, VirtAddr};
use crate::consts::LOWER_HALF_END;

static CONSUMED_USER_LAND: AtomicBool = AtomicBool::new(false);

pub struct EnterUserModeInput {
    pub rip: u64,
    pub rsp: u64,
    pub rflags: RFlags,
}

/// # Safety
/// Does 'sysretq'
/// Enable system call extension first
pub unsafe fn enter_user_mode(EnterUserModeInput { rip, rsp, rflags }: EnterUserModeInput) -> ! {
    let rflags = rflags.bits();
    unsafe {
        asm!("\
            mov rsp, {}
            sysretq",
            in(reg) rsp,
            in("rcx") rip,
            in("r11") rflags,
            // The user space program can only "return" with a `syscall`, which will jump to the syscall handler
            options(noreturn)
        )
    }
}

pub fn run_user_land() {
    // User land must only run once
    let previously_consumed = CONSUMED_USER_LAND.swap(false, Ordering::Relaxed);
    assert!(!previously_consumed);

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

    // Create new address space for user mode
    let memory = MEMORY.get().unwrap();
    let mut physical_memory = memory.physical_memory.lock();
    let mut user_l4 = memory.virtual_memory.lock().l4_mut().new_user(
        physical_memory
            .get_user_mode_frame_allocator()
            .allocate_frame_4kib()
            .unwrap(),
    );

    // Remove the module from physical memory map
    // We will only be using 4 KiB pages because most ELFs will have segments only aligned to 4 KiB, and Limine only aligns the ELF to 4 KiB
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
        }
    }

    // Map all non-referenced frames in the ELF as usable
    // Currently all non-referenced frames are gaps in the map
    // We just need to "fill the gaps" with usable
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
    // For simplicity we will just uncoditionally zero it
    {
        let start = module.addr().addr() + module.size() as usize;
        let ptr = NonNull::new(start as *mut u8).unwrap();
        let end = start.next_multiple_of(page_size.byte_len());
        let count = end - start;
        // Safety: we have exclusive accces to this memory
        unsafe { ptr.write_bytes(0, count) };
    }

    // Allocate a stack
    // Technically this stack placement could overlap with our ELF, but we will assume it won't
    let rsp = LOWER_HALF_END;
    {
        let stack_size: u64 = 64 * 0x400;
        // We are using 4 KiB pages because we need <2 MiB, but we could use any page size for the stack, as long as the stack size is a multiple of it
        let page_size = PageSize::_4KiB;
        let pages_len = stack_size.div_ceil(page_size.byte_len_u64());
        let start_page = Page::new(
            VirtAddr::new(rsp - pages_len * page_size.byte_len_u64()),
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
    };

    // Release memory lock
    drop(physical_memory);

    // Switch to the user address space
    // Safety: we can still reference kernel memory
    unsafe { user_l4.switch_to(memory.new_kernel_cr3_flags) };

    raw_syscall_handler::init();

    let input = EnterUserModeInput {
        rflags: RFlags::empty(),
        rip: entry_point.get(),
        rsp,
    };
    unsafe { enter_user_mode(input) };
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
