use crate::limine_requests::{MODULE_REQUEST, INIT_TASK_PATH};
use crate::memory::MEMORY;
use crate::memory::cpu_local_data::get_local;
use crate::memory::hhdm_offset::hhdm_offset;
use crate::memory::physical_memory::{MemoryType, OffsetMappedPhysAddr};
use crate::memory::vaddr_allocator::OffsetMappedVirtAddr;
use crate::task::task::{Task, TaskId, CpuContext};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;
use kernel_api_types::Priority;
use bitflags::bitflags;
use core::num::NonZero;
use core::ops::Range;
use core::ptr::{NonNull, slice_from_raw_parts_mut};
use elf::ElfBytes;
use elf::endian::AnyEndian;
use elf::segment::ProgramHeader;
use nodit::interval::ie;
use nodit::{Interval, NoditMap};
use crate::task::task::{VmaBacking, VmaEntry};
use num_enum::{IntoPrimitive, TryFromPrimitive};
use x86_64::{PhysAddr, VirtAddr};
use x86_64::structures::paging::{Mapper, OffsetPageTable, Page, PageSize, PageTable, PageTableFlags, PhysFrame, Size4KiB};
use crate::consts::LOWER_HALF_END;

/// Build a `PageTableFlags` from ELF segment flags, always setting PRESENT and USER_ACCESSIBLE.
fn elf_flags_to_page_table_flags(elf_flags: ElfSegmentFlags) -> PageTableFlags {
    let mut flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
    if elf_flags.contains(ElfSegmentFlags::WRITABLE) {
        flags |= PageTableFlags::WRITABLE;
    }
    if !elf_flags.contains(ElfSegmentFlags::EXECUTABLE) {
        flags |= PageTableFlags::NO_EXECUTE;
    }
    flags
}

/// Allocate a new user-mode L4 page table, zero it, copy kernel higher-half
/// entries (256..512), and return the L4 frame plus an `OffsetPageTable` mapper.
///
/// # Safety
/// The returned mapper borrows the page table with a `'static` lifetime.
/// The caller must ensure it does not outlive the physical frame.
unsafe fn create_user_page_table(
    phys: &mut crate::memory::physical_memory::PhysicalMemory,
) -> (PhysFrame<Size4KiB>, OffsetPageTable<'static>) {
    let l4_frame = phys
        .get_user_mode_frame_allocator()
        .allocate_frame_4kib()
        .expect("Failed to allocate L4 frame for user page table");

    let hhdm = VirtAddr::new(hhdm_offset().as_u64());
    let l4_virt = hhdm + l4_frame.start_address().as_u64();

    unsafe {
        // Zero the new page table
        l4_virt.as_mut_ptr::<PageTable>().write(PageTable::new());

        // Copy kernel higher-half entries from the kernel L4
        let memory = MEMORY.get().unwrap();
        let kernel_l4_virt = hhdm + memory.new_kernel_cr3.start_address().as_u64();
        let kernel_table = &*kernel_l4_virt.as_ptr::<PageTable>();
        let user_table = &mut *l4_virt.as_mut_ptr::<PageTable>();
        for i in 256..512 {
            user_table[i] = kernel_table[i].clone();
        }

        let mapper = OffsetPageTable::new(
            &mut *l4_virt.as_mut_ptr::<PageTable>(),
            hhdm,
        );
        (l4_frame, mapper)
    }
}

/// Create a user-mode task from the first Limine module matching INIT_TASK_PATH.
///
/// This parses the ELF, creates a new address space, maps ELF segments and a
/// user stack, then returns a `Task` ready to be scheduled.
pub fn create_user_task_from_elf(priority: Priority, parent_id: Option<TaskId>) -> Task {
    let module = MODULE_REQUEST
        .get_response()
        .unwrap()
        .modules()
        .iter()
        .find(|module| module.path() == INIT_TASK_PATH)
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
    let mut user_vmas: NoditMap<u64, Interval<u64>, VmaEntry> = NoditMap::new();

    // Create new address space for user mode
    let memory = MEMORY.get().unwrap();
    let mut physical_memory = memory.physical_memory.lock();
    let (l4_frame, mut mapper) = unsafe { create_user_page_table(&mut physical_memory) };
    let cr3 = l4_frame.start_address().as_u64();

    // Remove the module from physical memory map
    let page_size = Size4KiB::SIZE;
    let module_physical_interval = {
        let start = VirtAddr::from_ptr(module.addr()).offset_mapped().as_u64();
        ie(
            start,
            (start + module.size()).next_multiple_of(page_size),
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

        let start_page: Page<Size4KiB> = Page::containing_address(
            VirtAddr::new(segment.p_vaddr),
        );
        let file_pages_len = if segment.p_filesz > 0 {
            (segment.p_vaddr + segment.p_filesz).div_ceil(page_size)
                - segment.p_vaddr / page_size
        } else {
            0
        };
        let start_frame: PhysFrame<Size4KiB> = PhysFrame::containing_address(
            VirtAddr::from_ptr(module.addr()).offset_mapped() + segment.p_offset,
        );

        // Map the virtual memory to the ELF's frames
        let elf_flags = ElfSegmentFlags::from(segment);
        let flags = elf_flags_to_page_table_flags(elf_flags);
        for i in 0..file_pages_len {
            let page = start_page + i;
            let frame = start_frame + i;
            let mut frame_allocator = physical_memory.get_user_mode_frame_allocator();
            unsafe { mapper.map_to(page, frame, flags, &mut frame_allocator) }
                .unwrap()
                .ignore();
        }

        // Mark the ELF's frames as used by user mode
        if file_pages_len > 0 {
            let interval = {
                let start = start_frame.start_address().as_u64();
                ie(start, start + file_pages_len * page_size)
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
                    start..start.align_up(page_size)
                });
            }

            // We need to allocate, zero, and map additional frames.
            // bss_start_page = start_page + file_pages_len; we need pages from
            // that page up to (but not including) ceil((vaddr+memsz)/4K).
            // Using ceil((vaddr+filesz)/4K) as the subtrahend is wrong when
            // filesz=0 and vaddr is not page-aligned (ceil > floor = start_page).
            let extra_pages_len = (segment.p_vaddr + segment.p_memsz)
                .div_ceil(page_size)
                - start_page.start_address().as_u64() / page_size
                - file_pages_len;
            let extra_start_page = start_page + file_pages_len;
            for i in 0..extra_pages_len {
                let page = extra_start_page + i;
                let frame = physical_memory
                    .allocate_frame_with_type(MemoryType::UsedByUserMode)
                    .unwrap();
                let frame_ptr =
                    NonNull::new(frame.start_address().offset_mapped().as_mut_ptr::<u8>()).unwrap();
                // Safety: we own the frame
                unsafe { frame_ptr.write_bytes(0, page_size as usize) };
                let mut frame_allocator = physical_memory.get_user_mode_frame_allocator();
                unsafe { mapper.map_to(page, frame, flags, &mut frame_allocator) }
                    .unwrap()
                    .ignore();
            }
            total_pages += extra_pages_len;
        }

        // Track the mapped virtual address range
        if total_pages > 0 {
            let seg_start = start_page.start_address().as_u64();
            let seg_end = seg_start + total_pages * page_size;
            user_vmas
                .insert_strict(
                    ie(seg_start, seg_end),
                    VmaEntry { flags, backing: VmaBacking::EagerlyMapped },
                )
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
        let end = start.next_multiple_of(page_size as usize);
        let count = end - start;
        // Safety: we have exclusive access to this memory
        unsafe { ptr.write_bytes(0, count) };
    }

    // Register a lazy (demand-filled) user stack at the top of the canonical lower half.
    // LOWER_HALF_END is 0x7FFFFFFFFFFF (inclusive). Align down to page boundary
    // for the stack top, then subtract one page so RSP starts within mapped pages.
    let rsp = (LOWER_HALF_END + 1) - 0x1000;
    {
        let stack_size: u64 = 256 * 0x400;
        let pages_len = stack_size.div_ceil(page_size);
        let stack_start_vaddr = rsp - pages_len * page_size;
        let stack_flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE
            | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
        // No frame allocation — pages are zero-filled on first access.
        user_vmas
            .insert_strict(
                ie(stack_start_vaddr, stack_start_vaddr + pages_len * page_size),
                VmaEntry { flags: stack_flags, backing: VmaBacking::Anonymous },
            )
            .expect("user stack vaddr overlap");
    };

    // Release memory lock
    drop(physical_memory);

    // Get user segment selectors from the GDT
    let local = get_local();
    let gdt = local.gdt.get().unwrap();
    let user_cs = gdt.user_code_selector().0;
    let user_ss = gdt.user_data_selector().0;

    Task::new_user(entry_point.get(), rsp, l4_frame, cr3, user_cs, user_ss, user_vmas, 0, b"init_task", priority, parent_id)
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
pub fn create_user_task_from_elf_bytes(elf_bytes: &[u8], child_arg: u64, name: &[u8], priority: Priority, parent_id: Option<TaskId>) -> Result<Task, SpawnError> {
    let elf = ElfBytes::<AnyEndian>::minimal_parse(elf_bytes)
        .map_err(|_| {
            let magic = if elf_bytes.len() >= 4 { &elf_bytes[..4] } else { elf_bytes };
            log::warn!("create_user_task_from_elf_bytes: ELF parse failed, first 4 bytes: {:02x?}", magic);
            SpawnError::InvalidElf
        })?;

    let mut user_vmas: NoditMap<u64, Interval<u64>, VmaEntry> = NoditMap::new();

    let memory = MEMORY.get().unwrap();
    let mut physical_memory = memory.physical_memory.lock();
    let (l4_frame, mut mapper) = unsafe {
        create_user_page_table(&mut physical_memory)
    };
    let cr3 = l4_frame.start_address().as_u64();

    let page_size = Size4KiB::SIZE;

    // Map ELF LOAD segments
    for segment in elf.segments().ok_or(SpawnError::InvalidElf)? {
        if ElfSegmentType::try_from(segment.p_type) != Ok(ElfSegmentType::Load) {
            continue;
        }

        // Validate the segment references data within the ELF bytes
        if segment.p_offset + segment.p_filesz > elf_bytes.len() as u64 {
            return Err(SpawnError::InvalidElf);
        }

        let start_page: Page<Size4KiB> = Page::containing_address(
            VirtAddr::new(segment.p_vaddr),
        );

        let file_pages_len = if segment.p_filesz > 0 {
            (segment.p_vaddr + segment.p_filesz).div_ceil(page_size)
                - segment.p_vaddr / page_size
        } else {
            0
        };

        let elf_flags = ElfSegmentFlags::from(segment);
        let flags = elf_flags_to_page_table_flags(elf_flags);

        // Allocate fresh frames and copy file data for pages that contain file content
        for i in 0..file_pages_len {
            let page = start_page + i;
            let frame = physical_memory
                .allocate_frame_with_type(MemoryType::UsedByUserMode)
                .ok_or(SpawnError::OutOfMemory)?;

            // Zero the frame first, then copy the relevant bytes
            let frame_virt = frame.start_address().offset_mapped().as_mut_ptr::<u8>();
            let frame_virt = NonNull::new(frame_virt).unwrap();
            unsafe { frame_virt.write_bytes(0, page_size as usize) };

            // Calculate which bytes from the ELF to copy into this frame
            let page_vaddr = page.start_address().as_u64();
            let seg_file_start = segment.p_vaddr; // vaddr where file data starts
            let seg_file_end = segment.p_vaddr + segment.p_filesz;

            let copy_start_vaddr = page_vaddr.max(seg_file_start);
            let copy_end_vaddr = (page_vaddr + page_size).min(seg_file_end);

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
            unsafe { mapper.map_to(page, frame, flags, &mut frame_allocator) }
                .map_err(|_| SpawnError::OutOfMemory)?
                .ignore();
        }

        let mut total_pages = file_pages_len;

        // Handle BSS (p_memsz > p_filesz): allocate zeroed extra pages.
        // Same fix as above: use floor(vaddr/4K)+file_pages_len, not ceil((vaddr+filesz)/4K).
        if segment.p_memsz > segment.p_filesz {
            let extra_pages_len = (segment.p_vaddr + segment.p_memsz)
                .div_ceil(page_size)
                - start_page.start_address().as_u64() / page_size
                - file_pages_len;
            let bss_start_page = start_page + file_pages_len;
            for i in 0..extra_pages_len {
                let page = bss_start_page + i;
                let frame = physical_memory
                    .allocate_frame_with_type(MemoryType::UsedByUserMode)
                    .ok_or(SpawnError::OutOfMemory)?;
                let frame_ptr =
                    NonNull::new(frame.start_address().offset_mapped().as_mut_ptr::<u8>()).unwrap();
                unsafe { frame_ptr.write_bytes(0, page_size as usize) };
                let mut frame_allocator = physical_memory.get_user_mode_frame_allocator();
                unsafe { mapper.map_to(page, frame, flags, &mut frame_allocator) }
                    .map_err(|_| SpawnError::OutOfMemory)?
                    .ignore();
            }
            total_pages += extra_pages_len;
        }

        // Track the mapped virtual address range
        if total_pages > 0 {
            let seg_start = start_page.start_address().as_u64();
            let seg_end = seg_start + total_pages * page_size;
            user_vmas
                .insert_strict(
                    ie(seg_start, seg_end),
                    VmaEntry { flags, backing: VmaBacking::EagerlyMapped },
                )
                .expect("ELF segment vaddr overlap");
        }
    }

    // Parse entry point
    let entry_point = NonZero::new(elf.ehdr.e_entry).ok_or(SpawnError::InvalidElf)?;

    // Register a lazy (demand-filled) user stack at the top of the canonical lower half
    let rsp = (LOWER_HALF_END + 1) - 0x1000;
    {
        let stack_size: u64 = 256 * 0x400;
        let pages_len = stack_size.div_ceil(page_size);
        let stack_start_vaddr = rsp - pages_len * page_size;
        let stack_flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE
            | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
        // No frame allocation — pages are zero-filled on first access.
        user_vmas
            .insert_strict(
                ie(stack_start_vaddr, stack_start_vaddr + pages_len * page_size),
                VmaEntry { flags: stack_flags, backing: VmaBacking::Anonymous },
            )
            .expect("user stack vaddr overlap");
    }

    drop(physical_memory);

    let local = get_local();
    let gdt = local.gdt.get().unwrap();
    let user_cs = gdt.user_code_selector().0;
    let user_ss = gdt.user_data_selector().0;

    Ok(Task::new_user(entry_point.get(), rsp, l4_frame, cr3, user_cs, user_ss, user_vmas, child_arg, name, priority, parent_id))
}

/// Arguments passed via Box to the kernel loader task for async ELF loading.
///
/// Instead of copying the ELF into the kernel heap, we store a reference to the
/// parent's address space.  The loader walks the parent's page table via HHDM to
/// read ELF bytes directly from the parent's physical frames.
pub(crate) struct ElfLoaderArgs {
    pub stub_task: Arc<Task>,
    /// Physical address of the parent's L4 page table.
    pub parent_cr3: u64,
    /// User-space virtual address of the ELF buffer in the parent's address space.
    pub elf_user_ptr: u64,
    /// Length of the ELF buffer in bytes.
    pub elf_len: u64,
    pub child_arg: u64,
    pub name: [u8; 32],
    pub name_len: u8,
    pub priority: u8,
    pub parent_id: TaskId,
}

/// Read bytes from the parent's address space via HHDM page-table walk.
///
/// Copies `count` bytes from `parent_vaddr` (in the parent's user address space)
/// into `dst`.  Returns the number of bytes actually copied (may be less than
/// `count` if a page is not mapped).
fn read_from_parent(
    parent_l4_frame: PhysFrame,
    parent_vaddr: u64,
    dst: &mut [u8],
) -> usize {
    use crate::memory::page_tables::get_kernel_vaddr_from_user_vaddr;

    let mut copied = 0usize;
    let mut src = parent_vaddr;
    while copied < dst.len() {
        let kva = match get_kernel_vaddr_from_user_vaddr(
            parent_l4_frame,
            VirtAddr::new(src),
        ) {
            Some(v) => v,
            None => break,
        };

        // Copy up to the end of the current 4 KiB page.
        let page_remaining = (Size4KiB::SIZE - (src % Size4KiB::SIZE)) as usize;
        let to_copy = page_remaining.min(dst.len() - copied);
        unsafe {
            core::ptr::copy_nonoverlapping(
                kva.as_ptr::<u8>(),
                dst.as_mut_ptr().add(copied),
                to_copy,
            );
        }
        copied += to_copy;
        src += to_copy as u64;
    }
    copied
}

/// Fill a Loading stub with a parsed ELF address space.
///
/// Reads the ELF directly from the parent's physical pages (via HHDM) instead
/// of from a kernel-heap copy.  The parent must keep its ELF buffer mapped
/// until this function returns (i.e. until `sys_wait_task_ready` succeeds).
///
/// On success, `stub.cr3` is set with `Ordering::Release` and `stub.inner`
/// holds the complete context, page table, and VMAs.
pub(crate) fn fill_loading_task(
    stub: &Arc<Task>,
    parent_cr3: u64,
    elf_user_ptr: u64,
    elf_len: u64,
    child_arg: u64,
    _name: [u8; 32],
    _name_len: u8,
    _priority: u8,
    _parent_id: TaskId,
) -> Result<(), SpawnError> {
    let parent_l4_frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(parent_cr3));

    // ── Parse ELF header (64 bytes) from parent pages ──────────────────────
    if elf_len < 64 {
        return Err(SpawnError::InvalidElf);
    }
    let mut ehdr_buf = [0u8; 64];
    if read_from_parent(parent_l4_frame, elf_user_ptr, &mut ehdr_buf) != 64 {
        return Err(SpawnError::InvalidElf);
    }
    // Validate ELF magic
    if &ehdr_buf[0..4] != b"\x7fELF" {
        log::warn!("fill_loading_task: bad ELF magic {:02x?}", &ehdr_buf[0..4]);
        return Err(SpawnError::InvalidElf);
    }
    // 64-bit ELF header fields (little-endian)
    let e_entry   = u64::from_le_bytes(ehdr_buf[24..32].try_into().unwrap());
    let e_phoff   = u64::from_le_bytes(ehdr_buf[32..40].try_into().unwrap());
    let e_phentsize = u16::from_le_bytes(ehdr_buf[54..56].try_into().unwrap()) as u64;
    let e_phnum     = u16::from_le_bytes(ehdr_buf[56..58].try_into().unwrap()) as u64;

    // ── Read program headers from parent pages ──────────────────────────────
    let phdrs_size = (e_phnum * e_phentsize) as usize;
    if phdrs_size > 0x10000 {
        return Err(SpawnError::InvalidElf); // sanity limit
    }
    let mut phdrs_buf = alloc::vec![0u8; phdrs_size];
    if read_from_parent(parent_l4_frame, elf_user_ptr + e_phoff, &mut phdrs_buf) != phdrs_size {
        return Err(SpawnError::InvalidElf);
    }

    // Parse program headers into a small vec of (p_type, p_offset, p_vaddr, p_filesz, p_memsz, p_flags)
    struct PhdrInfo { p_type: u32, p_flags: u32, p_offset: u64, p_vaddr: u64, p_filesz: u64, p_memsz: u64 }
    let mut phdrs = Vec::with_capacity(e_phnum as usize);
    for i in 0..e_phnum as usize {
        let off = i * e_phentsize as usize;
        let h = &phdrs_buf[off..off + e_phentsize as usize];
        phdrs.push(PhdrInfo {
            p_type:  u32::from_le_bytes(h[0..4].try_into().unwrap()),
            p_flags: u32::from_le_bytes(h[4..8].try_into().unwrap()),
            p_offset: u64::from_le_bytes(h[8..16].try_into().unwrap()),
            p_vaddr:  u64::from_le_bytes(h[16..24].try_into().unwrap()),
            p_filesz: u64::from_le_bytes(h[32..40].try_into().unwrap()),
            p_memsz:  u64::from_le_bytes(h[40..48].try_into().unwrap()),
        });
    }

    let entry_point = core::num::NonZero::new(e_entry).ok_or(SpawnError::InvalidElf)?;

    // ── Helper: lock physical_memory with interrupts disabled ────────────
    // The loader runs as a kernel task with interrupts ENABLED. physical_memory
    // is a spinlock; if the timer preempts us while holding it, any same-CPU
    // syscall that also needs physical_memory will spin with interrupts
    // disabled → deadlock. We disable interrupts for each short critical
    // section, keeping them enabled during the slow `read_from_parent` calls
    // so the timer can still preempt us between pages.
    let memory = MEMORY.get().unwrap();

    // Create child page table (short critical section)
    let (l4_frame, mut mapper) = x86_64::instructions::interrupts::without_interrupts(|| {
        let mut pm = memory.physical_memory.lock();
        unsafe { create_user_page_table(&mut pm) }
    });

    let page_size = Size4KiB::SIZE;
    let mut user_vmas: NoditMap<u64, Interval<u64>, VmaEntry> = NoditMap::new();

    // Map ELF LOAD segments — read data from parent's pages via HHDM.
    // Each page is processed in three steps:
    //   1. Allocate frame (interrupts off, lock held — fast)
    //   2. Zero + read from parent (interrupts on, no lock — slow)
    //   3. Map frame into child page table (interrupts off, lock held — fast)
    for phdr in &phdrs {
        // PT_LOAD = 1
        if phdr.p_type != 1 {
            continue;
        }

        if phdr.p_offset + phdr.p_filesz > elf_len {
            return Err(SpawnError::InvalidElf);
        }

        let start_page: Page<Size4KiB> = Page::containing_address(
            VirtAddr::new(phdr.p_vaddr),
        );

        let file_pages_len = if phdr.p_filesz > 0 {
            (phdr.p_vaddr + phdr.p_filesz).div_ceil(page_size)
                - phdr.p_vaddr / page_size
        } else {
            0
        };

        let elf_flags = ElfSegmentFlags::from_bits_retain(phdr.p_flags);
        let flags = elf_flags_to_page_table_flags(elf_flags);

        for i in 0..file_pages_len {
            let page = start_page + i;

            // Step 1: allocate frame (short critical section)
            let frame = x86_64::instructions::interrupts::without_interrupts(|| {
                memory.physical_memory.lock()
                    .allocate_frame_with_type(MemoryType::UsedByUserMode)
            }).ok_or(SpawnError::OutOfMemory)?;

            // Step 2: zero + read from parent (no lock, interrupts enabled)
            let frame_virt = frame.start_address().offset_mapped().as_mut_ptr::<u8>();
            let frame_virt = NonNull::new(frame_virt).unwrap();
            unsafe { frame_virt.write_bytes(0, page_size as usize) };

            let page_vaddr = page.start_address().as_u64();
            let seg_file_start = phdr.p_vaddr;
            let seg_file_end = phdr.p_vaddr + phdr.p_filesz;
            let copy_start_vaddr = page_vaddr.max(seg_file_start);
            let copy_end_vaddr = (page_vaddr + page_size).min(seg_file_end);

            if copy_start_vaddr < copy_end_vaddr {
                let offset_in_page = (copy_start_vaddr - page_vaddr) as usize;
                let offset_in_file = phdr.p_offset + (copy_start_vaddr - phdr.p_vaddr);
                let count = (copy_end_vaddr - copy_start_vaddr) as usize;

                let dst = unsafe {
                    core::slice::from_raw_parts_mut(
                        frame_virt.as_ptr().add(offset_in_page),
                        count,
                    )
                };
                let copied = read_from_parent(
                    parent_l4_frame,
                    elf_user_ptr + offset_in_file,
                    dst,
                );
                if copied != count {
                    return Err(SpawnError::InvalidElf);
                }
            }

            // Step 3: map frame (short critical section)
            x86_64::instructions::interrupts::without_interrupts(|| {
                let mut pm = memory.physical_memory.lock();
                let mut fa = pm.get_user_mode_frame_allocator();
                unsafe { mapper.map_to(page, frame, flags, &mut fa) }
            }).map_err(|_| SpawnError::OutOfMemory)?.ignore();
        }

        let mut total_pages = file_pages_len;

        // BSS pages (p_memsz > p_filesz): allocate + zero + map
        if phdr.p_memsz > phdr.p_filesz {
            let extra_pages_len = (phdr.p_vaddr + phdr.p_memsz)
                .div_ceil(page_size)
                - start_page.start_address().as_u64() / page_size
                - file_pages_len;
            let bss_start_page = start_page + file_pages_len;
            for i in 0..extra_pages_len {
                let page = bss_start_page + i;
                // BSS pages are just zeroed — allocate + zero + map in one short section
                x86_64::instructions::interrupts::without_interrupts(|| -> Result<(), SpawnError> {
                    let mut pm = memory.physical_memory.lock();
                    let frame = pm
                        .allocate_frame_with_type(MemoryType::UsedByUserMode)
                        .ok_or(SpawnError::OutOfMemory)?;
                    let frame_ptr =
                        NonNull::new(frame.start_address().offset_mapped().as_mut_ptr::<u8>()).unwrap();
                    unsafe { frame_ptr.write_bytes(0, page_size as usize) };
                    let mut fa = pm.get_user_mode_frame_allocator();
                    unsafe { mapper.map_to(page, frame, flags, &mut fa) }
                        .map_err(|_| SpawnError::OutOfMemory)?
                        .ignore();
                    Ok(())
                })?;
            }
            total_pages += extra_pages_len;
        }

        if total_pages > 0 {
            let seg_start = start_page.start_address().as_u64();
            let seg_end = seg_start + total_pages * page_size;
            user_vmas
                .insert_strict(
                    ie(seg_start, seg_end),
                    VmaEntry { flags, backing: VmaBacking::EagerlyMapped },
                )
                .expect("ELF segment vaddr overlap");
        }
    }

    // Register user stack VMA
    let rsp = (crate::consts::LOWER_HALF_END + 1) - 0x1000;
    {
        let stack_size: u64 = 256 * 0x400;
        let pages_len = stack_size.div_ceil(page_size);
        let stack_start_vaddr = rsp - pages_len * page_size;
        let stack_flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE
            | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
        user_vmas
            .insert_strict(
                ie(stack_start_vaddr, stack_start_vaddr + pages_len * page_size),
                VmaEntry { flags: stack_flags, backing: VmaBacking::Anonymous },
            )
            .expect("user stack vaddr overlap");
    }

    let local = get_local();
    let gdt = local.gdt.get().unwrap();
    let user_cs = gdt.user_code_selector().0;
    let user_ss = gdt.user_data_selector().0;

    // Fill the stub's inner (context, page table, VMAs).
    // kernel_stack and kernel_stack_top were already set by new_loading().
    {
        let mut inner = stub.inner.lock();
        inner.context = CpuContext {
            r15: 0, r14: 0, r13: 0, r12: 0, r11: 0, r10: 0, r9: 0, r8: 0,
            rdi: child_arg, rsi: 0, rbp: 0, rbx: 0, rdx: 0, rcx: 0, rax: 0,
            rip: entry_point.get(),
            cs: user_cs as u64,
            rflags: 0x200,
            rsp,
            ss: user_ss as u64,
        };
        inner.user_page_table = Some(l4_frame);
        inner.user_vmas = user_vmas;
    }
    stub.cr3.store(l4_frame.start_address().as_u64(), Ordering::Release);

    Ok(())
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
