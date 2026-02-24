use crate::memory::MEMORY;
use crate::memory::cpu_local_data::get_local;
use crate::memory::hhdm_offset::hhdm_offset;
use crate::memory::physical_memory::{MemoryType, OffsetMappedPhysAddr, PhysicalMemory};
use crate::memory::user_vaddr;
use crate::task::task::TaskKind;
use kernel_api_types::{MMAP_EXEC, MMAP_WRITE};
use x86_64::structures::paging::{
    Mapper, OffsetPageTable, Page, PageSize, PageTable, PageTableFlags, PhysFrame, Size4KiB,
};
use x86_64::{PhysAddr, VirtAddr};

/// Syscall: allocate virtual memory for the calling user task.
///
/// Arguments: size (bytes), flags (MMAP_WRITE | MMAP_EXEC)
/// Returns: start virtual address, or 0 on failure.
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

    let start_vaddr = match user_vaddr::allocate_user_pages(&mut inner.user_vaddr_set, n_pages) {
        Some(addr) => addr,
        None => return 0,
    };

    let mut page_flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
    if (flags & MMAP_WRITE) != 0 {
        page_flags |= PageTableFlags::WRITABLE;
    }
    if (flags & MMAP_EXEC) == 0 {
        page_flags |= PageTableFlags::NO_EXECUTE;
    }

    let hhdm_offset = hhdm_offset();
    let user_l4_frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(task.cr3));
    let l4_virt_addr = VirtAddr::new(hhdm_offset.as_u64() + user_l4_frame.start_address().as_u64());
    let l4_table = unsafe { &mut *l4_virt_addr.as_mut_ptr::<PageTable>() };
    let mut mapper = unsafe { OffsetPageTable::new(l4_table, VirtAddr::new(hhdm_offset.as_u64())) };

    let memory = MEMORY.get().unwrap();
    let mut physical_memory = memory.physical_memory.lock();

    for i in 0..n_pages {
        let vaddr = VirtAddr::new(start_vaddr + i * Size4KiB::SIZE);
        let page: Page<Size4KiB> = Page::containing_address(vaddr);

        let frame = match physical_memory.allocate_frame_with_type(MemoryType::UsedByUserMode) {
            Some(f) => f,
            None => {
                rollback_mmap(&mut mapper, &mut physical_memory, start_vaddr, i);
                user_vaddr::free_user_pages(&mut inner.user_vaddr_set, start_vaddr, n_pages * Size4KiB::SIZE);
                return 0;
            }
        };

        // Security: zero the frame before giving it to user space
        let frame_virt = frame.start_address().offset_mapped();
        unsafe {
            core::ptr::write_bytes(frame_virt.as_mut_ptr::<u8>(), 0, Size4KiB::SIZE as usize);
        }

        let mut frame_allocator = physical_memory.get_user_mode_frame_allocator();
        let map_result = unsafe { mapper.map_to(page, frame, page_flags, &mut frame_allocator) };
        drop(frame_allocator);

        if map_result.is_err() {
            let _ = physical_memory.free_frame(frame, MemoryType::UsedByUserMode);
            rollback_mmap(&mut mapper, &mut physical_memory, start_vaddr, i);
            user_vaddr::free_user_pages(&mut inner.user_vaddr_set, start_vaddr, n_pages * Size4KiB::SIZE);
            return 0;
        }
    }

    start_vaddr
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
    if !user_vaddr::free_user_pages(&mut inner.user_vaddr_set, addr, total_size) {
        return !0u64;
    }

    let hhdm_offset = hhdm_offset();
    let user_l4_frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(task.cr3));
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
