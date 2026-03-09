use crate::memory::MEMORY;
use crate::memory::cpu_local_data::get_local;
use crate::memory::hhdm_offset::hhdm_offset;
use crate::memory::physical_memory::{MemoryType, OffsetMappedPhysAddr, PhysicalMemory};
use crate::memory::user_vaddr;
use crate::task::task::{TaskKind, VmaBacking, VmaEntry};
use core::sync::atomic::Ordering;
use kernel_api_types::{MMAP_EXEC, MMAP_WRITE, MREMAP_MAYMOVE};
use nodit::interval::ii;
use x86_64::structures::paging::mapper::{MappedFrame, MapToError, Translate, TranslateResult};
use x86_64::structures::paging::{
    Mapper, OffsetPageTable, Page, PageSize, PageTable, PageTableFlags, PhysFrame, Size4KiB,
};
use x86_64::{PhysAddr, VirtAddr};

/// Syscall: allocate virtual memory for the calling user task (lazy / anonymous).
///
/// Arguments: size (bytes), flags (MMAP_WRITE | MMAP_EXEC)
/// Returns: start virtual address, or 0 on failure.
/// Frames are not allocated now; they are zero-filled on the first access.
pub fn sys_mmap(size: u64, flags: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    if size == 0 {
        return 0;
    }

    let n_pages = size.div_ceil(Size4KiB::SIZE);

    let cpu = get_local();
    let task = {
        let rq = cpu.run_queue.get().unwrap().lock();
        match &rq.current_task {
            Some(t) if t.kind == TaskKind::User => t.clone(),
            _ => return 0,
        }
    };

    let mut inner = task.inner.lock();

    let mut page_flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
    if (flags & MMAP_WRITE) != 0 {
        page_flags |= PageTableFlags::WRITABLE;
    }
    if (flags & MMAP_EXEC) == 0 {
        page_flags |= PageTableFlags::NO_EXECUTE;
    }

    let entry = VmaEntry { flags: page_flags, backing: VmaBacking::Anonymous };
    match user_vaddr::allocate_user_vma(&mut inner.user_vmas, n_pages, entry) {
        Some(addr) => addr,
        None => 0,
    }
}

/// Syscall: unmap and free virtual memory pages previously allocated with sys_mmap.
///
/// Arguments: addr (start virtual address), size (bytes)
/// Returns: 0 on success, !0 on failure.
pub fn sys_munmap(addr: u64, size: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    if size == 0 || addr % Size4KiB::SIZE != 0 {
        return !0u64;
    }

    let n_pages = size.div_ceil(Size4KiB::SIZE);

    let cpu = get_local();
    let task = {
        let rq = cpu.run_queue.get().unwrap().lock();
        match &rq.current_task {
            Some(t) if t.kind == TaskKind::User => t.clone(),
            _ => return !0u64,
        }
    };

    let mut inner = task.inner.lock();

    let total_size = n_pages * Size4KiB::SIZE;
    if !user_vaddr::free_user_vma(&mut inner.user_vmas, addr, total_size) {
        return !0u64;
    }

    let hhdm_offset = hhdm_offset();
    let user_l4_frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(task.cr3.load(Ordering::Relaxed)));
    let l4_virt_addr = VirtAddr::new(hhdm_offset.as_u64() + user_l4_frame.start_address().as_u64());
    let l4_table = unsafe { &mut *l4_virt_addr.as_mut_ptr::<PageTable>() };
    let mut mapper = unsafe { OffsetPageTable::new(l4_table, VirtAddr::new(hhdm_offset.as_u64())) };

    let memory = MEMORY.get().unwrap();
    let mut physical_memory = memory.physical_memory.lock();

    for i in 0..n_pages {
        let vaddr = VirtAddr::new(addr + i * Size4KiB::SIZE);
        let page: Page<Size4KiB> = Page::containing_address(vaddr);

        if let Ok((frame, _, flush)) = mapper.unmap(page) {
            flush.flush();
            let _ = physical_memory.free_frame(frame, MemoryType::UsedByUserMode);
        }
    }

    0
}

/// Syscall: allocate a shared physical buffer and map it into the caller's address space.
///
/// Arguments: size (bytes), vaddr_out_ptr
/// Returns: SharedBufId, or u64::MAX on failure.
/// Writes the mapped virtual address to `vaddr_out_ptr`.
pub fn sys_create_shared_buf(size: u64, vaddr_out_ptr: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    if size == 0 || !super::validate_user_ptr(vaddr_out_ptr, 8) {
        return u64::MAX;
    }

    let n_pages = size.div_ceil(Size4KiB::SIZE);

    let cpu = get_local();
    let task = {
        let rq = cpu.run_queue.get().unwrap().lock();
        match &rq.current_task {
            Some(t) if t.kind == TaskKind::User => t.clone(),
            _ => return u64::MAX,
        }
    };

    match crate::shared_buf::create_shared_buf(&task, n_pages) {
        Some((id, vaddr)) => {
            unsafe { core::ptr::write(vaddr_out_ptr as *mut u64, vaddr) };
            id
        }
        None => u64::MAX,
    }
}

/// Syscall: map an existing shared buffer into the caller's address space.
///
/// Arguments: shared_buf_id
/// Returns: start virtual address, or 0 on failure.
pub fn sys_map_shared_buf(id: u64, _: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    let cpu = get_local();
    let task = {
        let rq = cpu.run_queue.get().unwrap().lock();
        match &rq.current_task {
            Some(t) if t.kind == TaskKind::User => t.clone(),
            _ => return 0,
        }
    };

    match crate::shared_buf::map_shared_buf(id, &task) {
        Some(vaddr) => vaddr,
        None => 0,
    }
}

/// Syscall: free the physical pages backing a shared buffer.
///
/// Arguments: shared_buf_id
/// Returns: 0 (always succeeds; no-op if ID unknown).
pub fn sys_destroy_shared_buf(id: u64, _: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    crate::shared_buf::destroy_shared_buf(id);
    0
}

/// Syscall: change protection flags on an already-mapped range.
///
/// Arguments: addr (page-aligned), size (bytes), flags (MMAP_WRITE | MMAP_EXEC)
/// Returns: 0 on success, !0 on failure.
pub fn sys_mprotect(addr: u64, size: u64, flags: u64, _: u64, _: u64, _: u64) -> u64 {
    if addr % Size4KiB::SIZE != 0 || size == 0 {
        return !0u64;
    }

    let n_pages = size.div_ceil(Size4KiB::SIZE);
    let total = n_pages * Size4KiB::SIZE;

    let cpu = get_local();
    let task = {
        let rq = cpu.run_queue.get().unwrap().lock();
        match &rq.current_task {
            Some(t) if t.kind == TaskKind::User => t.clone(),
            _ => return !0u64,
        }
    };

    let mut inner = task.inner.lock();

    if !user_vaddr::is_user_vaddr_valid_range(
        &inner.user_vmas,
        VirtAddr::new(addr),
        VirtAddr::new(addr + total),
    ) {
        return !0u64;
    }

    let mut new_page_flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
    if (flags & MMAP_WRITE) != 0 {
        new_page_flags |= PageTableFlags::WRITABLE;
    }
    if (flags & MMAP_EXEC) == 0 {
        new_page_flags |= PageTableFlags::NO_EXECUTE;
    }

    // Update VMA flags so future demand-fills use the new protection.
    for (_, entry) in inner.user_vmas.overlapping_mut(ii(addr, addr + total - 1)) {
        entry.flags = new_page_flags;
    }

    let hhdm_offset = hhdm_offset();
    let user_l4_frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(task.cr3.load(Ordering::Relaxed)));
    let l4_virt_addr = VirtAddr::new(hhdm_offset.as_u64() + user_l4_frame.start_address().as_u64());
    let l4_table = unsafe { &mut *l4_virt_addr.as_mut_ptr::<PageTable>() };
    let mut mapper = unsafe { OffsetPageTable::new(l4_table, VirtAddr::new(hhdm_offset.as_u64())) };

    for i in 0..n_pages {
        let vaddr = VirtAddr::new(addr + i * Size4KiB::SIZE);
        let page: Page<Size4KiB> = Page::containing_address(vaddr);
        if let Ok(flush) = unsafe { mapper.update_flags(page, new_page_flags) } {
            flush.flush();
        }
    }

    0
}

/// Syscall: resize an mmap allocation.
///
/// Arguments: old_addr, old_size, new_size, flags (MREMAP_MAYMOVE)
/// Returns: new start virtual address on success (may equal old_addr), 0 on failure.
pub fn sys_mremap(old_addr: u64, old_size: u64, new_size: u64, flags: u64, _: u64, _: u64) -> u64 {
    if old_addr % Size4KiB::SIZE != 0 || old_size == 0 || new_size == 0 {
        return 0;
    }

    let old_pages = old_size.div_ceil(Size4KiB::SIZE);
    let new_pages = new_size.div_ceil(Size4KiB::SIZE);
    let old_total = old_pages * Size4KiB::SIZE;
    let new_total = new_pages * Size4KiB::SIZE;

    let cpu = get_local();
    let task = {
        let rq = cpu.run_queue.get().unwrap().lock();
        match &rq.current_task {
            Some(t) if t.kind == TaskKind::User => t.clone(),
            _ => return 0,
        }
    };

    let mut inner = task.inner.lock();

    if !user_vaddr::is_user_vaddr_valid_range(
        &inner.user_vmas,
        VirtAddr::new(old_addr),
        VirtAddr::new(old_addr + old_total),
    ) {
        return 0;
    }

    // Read preserved flags from the VMA (works even if the page is not yet present).
    let preserved_flags = match user_vaddr::lookup_vma(&inner.user_vmas, old_addr) {
        Some(e) => e.flags,
        None => return 0,
    };

    // No-op
    if new_pages == old_pages {
        return old_addr;
    }

    let hhdm_offset = hhdm_offset();
    let user_l4_frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(task.cr3.load(Ordering::Relaxed)));
    let l4_virt_addr = VirtAddr::new(hhdm_offset.as_u64() + user_l4_frame.start_address().as_u64());
    let l4_table = unsafe { &mut *l4_virt_addr.as_mut_ptr::<PageTable>() };
    let mut mapper = unsafe { OffsetPageTable::new(l4_table, VirtAddr::new(hhdm_offset.as_u64())) };

    let memory = MEMORY.get().unwrap();
    let mut physical_memory = memory.physical_memory.lock();

    // Shrink: unmap + free tail pages, trim VMA map.
    if new_pages < old_pages {
        let tail_start = old_addr + new_total;
        let tail_pages = old_pages - new_pages;
        for i in 0..tail_pages {
            let vaddr = VirtAddr::new(tail_start + i * Size4KiB::SIZE);
            let page: Page<Size4KiB> = Page::containing_address(vaddr);
            if let Ok((frame, _, flush)) = mapper.unmap(page) {
                flush.flush();
                let _ = physical_memory.free_frame(frame, MemoryType::UsedByUserMode);
            }
        }
        let _ = inner.user_vmas.cut(&ii(tail_start, old_addr + old_total - 1)).count();
        return old_addr;
    }

    // Grow: new_pages > old_pages
    let ext_start = old_addr + old_total;
    let ext_size = (new_pages - old_pages) * Size4KiB::SIZE;

    if user_vaddr::is_range_free(&inner.user_vmas, ext_start, ext_size) {
        // Grow in-place: register the extension as an Anonymous VMA; no frames yet.
        let _ = inner.user_vmas.insert_overwrite(
            nodit::interval::ie(ext_start, old_addr + new_total),
            VmaEntry { flags: preserved_flags, backing: VmaBacking::Anonymous },
        );
        return old_addr;
    }

    // In-place failed — relocate if MREMAP_MAYMOVE.
    if (flags & MREMAP_MAYMOVE) == 0 {
        return 0;
    }

    // Allocate new VMA (Anonymous — pages arrive on demand).
    let new_addr = match user_vaddr::allocate_user_vma(
        &mut inner.user_vmas,
        new_pages,
        VmaEntry { flags: preserved_flags, backing: VmaBacking::Anonymous },
    ) {
        Some(addr) => addr,
        None => return 0,
    };

    // Copy only present source pages; absent ones will zero-fill on demand in the new VMA.
    for i in 0..old_pages {
        let old_vaddr = VirtAddr::new(old_addr + i * Size4KiB::SIZE);
        let old_frame = match mapper.translate(old_vaddr) {
            TranslateResult::Mapped { frame: MappedFrame::Size4KiB(f), .. } => f,
            _ => continue, // not present — will be zero-filled on demand
        };

        let new_vaddr = VirtAddr::new(new_addr + i * Size4KiB::SIZE);
        let new_page: Page<Size4KiB> = Page::containing_address(new_vaddr);

        let new_frame = match physical_memory.allocate_frame_with_type(MemoryType::UsedByUserMode) {
            Some(f) => f,
            None => {
                rollback_mmap(&mut mapper, &mut physical_memory, new_addr, i);
                let _ = user_vaddr::free_user_vma(&mut inner.user_vmas, new_addr, new_total);
                return 0;
            }
        };

        // Copy via HHDM
        let src = (hhdm_offset.as_u64() + old_frame.start_address().as_u64()) as *const u8;
        let dst = new_frame.start_address().offset_mapped().as_mut_ptr::<u8>();
        unsafe { core::ptr::copy_nonoverlapping(src, dst, Size4KiB::SIZE as usize) };

        let mut frame_allocator = physical_memory.get_user_mode_frame_allocator();
        let map_result = unsafe { mapper.map_to(new_page, new_frame, preserved_flags, &mut frame_allocator) };
        drop(frame_allocator);

        if let Err(MapToError::PageAlreadyMapped(_)) = map_result {
            // Shouldn't happen, but safe to ignore
            let _ = physical_memory.free_frame(new_frame, MemoryType::UsedByUserMode);
        } else if map_result.is_err() {
            let _ = physical_memory.free_frame(new_frame, MemoryType::UsedByUserMode);
            rollback_mmap(&mut mapper, &mut physical_memory, new_addr, i);
            let _ = user_vaddr::free_user_vma(&mut inner.user_vmas, new_addr, new_total);
            return 0;
        } else if let Ok(flush) = map_result {
            flush.flush();
        }
    }

    // Unmap and free old present pages.
    for i in 0..old_pages {
        let vaddr = VirtAddr::new(old_addr + i * Size4KiB::SIZE);
        let page: Page<Size4KiB> = Page::containing_address(vaddr);
        if let Ok((frame, _, flush)) = mapper.unmap(page) {
            flush.flush();
            let _ = physical_memory.free_frame(frame, MemoryType::UsedByUserMode);
        }
    }
    let _ = user_vaddr::free_user_vma(&mut inner.user_vmas, old_addr, old_total);

    new_addr
}

fn rollback_mmap(
    mapper: &mut OffsetPageTable,
    physical_memory: &mut PhysicalMemory,
    start_vaddr: u64,
    count: u64,
) {
    for j in 0..count {
        let vaddr = VirtAddr::new(start_vaddr + j * Size4KiB::SIZE);
        let page: Page<Size4KiB> = Page::containing_address(vaddr);
        if let Ok((frame, _, flush)) = mapper.unmap(page) {
            flush.flush();
            let _ = physical_memory.free_frame(frame, MemoryType::UsedByUserMode);
        }
    }
}
