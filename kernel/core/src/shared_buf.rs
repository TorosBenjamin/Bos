use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use x86_64::structures::paging::{
    Mapper, OffsetPageTable, Page, PageSize, PageTable, PageTableFlags, PhysFrame, Size4KiB,
};
use x86_64::{PhysAddr, VirtAddr};

use crate::memory::MEMORY;
use crate::memory::hhdm_offset::hhdm_offset;
use crate::memory::physical_memory::{MemoryType, OffsetMappedPhysAddr};
use crate::memory::user_vaddr;
use crate::task::task::Task;

pub type SharedBufId = u64;

struct SharedBuf {
    frames: Vec<PhysFrame<Size4KiB>>,
}

static NEXT_BUF_ID: AtomicU64 = AtomicU64::new(1);
static SHARED_BUF_REGISTRY: Mutex<BTreeMap<SharedBufId, SharedBuf>> =
    Mutex::new(BTreeMap::new());

/// Allocate `n_pages` physical pages tagged `SharedBuffer`, map them into `task`'s
/// address space, and register them in the global registry.
///
/// Returns `(id, start_vaddr)` or `None` on allocation failure.
pub fn create_shared_buf(task: &Task, n_pages: u64) -> Option<(SharedBufId, u64)> {
    let mut inner = task.inner.lock();

    let start_vaddr = user_vaddr::allocate_user_pages(&mut inner.user_vaddr_set, n_pages)?;

    let page_flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::USER_ACCESSIBLE
        | PageTableFlags::NO_EXECUTE;

    let hhdm = hhdm_offset();
    let user_l4_frame =
        PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(task.cr3));
    let l4_virt =
        VirtAddr::new(hhdm.as_u64() + user_l4_frame.start_address().as_u64());
    let l4_table = unsafe { &mut *l4_virt.as_mut_ptr::<PageTable>() };
    let mut mapper =
        unsafe { OffsetPageTable::new(l4_table, VirtAddr::new(hhdm.as_u64())) };

    let memory = MEMORY.get().unwrap();
    let mut phys_mem = memory.physical_memory.lock();

    let mut frames: Vec<PhysFrame<Size4KiB>> = Vec::new();

    for i in 0..n_pages {
        let vaddr = VirtAddr::new(start_vaddr + i * Size4KiB::SIZE);
        let page: Page<Size4KiB> = Page::containing_address(vaddr);

        let frame = match phys_mem.allocate_frame_with_type(MemoryType::SharedBuffer) {
            Some(f) => f,
            None => {
                rollback(&mut mapper, &mut phys_mem, start_vaddr, i, true);
                user_vaddr::free_user_pages(
                    &mut inner.user_vaddr_set,
                    start_vaddr,
                    n_pages * Size4KiB::SIZE,
                );
                return None;
            }
        };

        // Zero before handing to user space
        unsafe {
            core::ptr::write_bytes(
                frame.start_address().offset_mapped().as_mut_ptr::<u8>(),
                0,
                Size4KiB::SIZE as usize,
            );
        }

        let mut pt_alloc = phys_mem.get_user_mode_frame_allocator();
        let result = unsafe { mapper.map_to(page, frame, page_flags, &mut pt_alloc) };
        drop(pt_alloc);

        if result.is_err() {
            let _ = phys_mem.free_frame(frame, MemoryType::SharedBuffer);
            rollback(&mut mapper, &mut phys_mem, start_vaddr, i, true);
            user_vaddr::free_user_pages(
                &mut inner.user_vaddr_set,
                start_vaddr,
                n_pages * Size4KiB::SIZE,
            );
            return None;
        }

        frames.push(frame);
    }

    let id = NEXT_BUF_ID.fetch_add(1, Ordering::Relaxed);
    SHARED_BUF_REGISTRY.lock().insert(id, SharedBuf { frames });

    Some((id, start_vaddr))
}

/// Map an existing shared buffer into `task`'s address space.
///
/// Returns the start virtual address, or `None` if the ID is unknown or
/// address-space allocation fails.
pub fn map_shared_buf(id: SharedBufId, task: &Task) -> Option<u64> {
    // Snapshot the frame list while holding the registry lock briefly.
    let frames: Vec<PhysFrame<Size4KiB>> = {
        let registry = SHARED_BUF_REGISTRY.lock();
        registry.get(&id)?.frames.clone()
    };
    let n_pages = frames.len() as u64;

    let mut inner = task.inner.lock();

    let start_vaddr =
        user_vaddr::allocate_user_pages(&mut inner.user_vaddr_set, n_pages)?;

    let page_flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::USER_ACCESSIBLE
        | PageTableFlags::NO_EXECUTE;

    let hhdm = hhdm_offset();
    let user_l4_frame =
        PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(task.cr3));
    let l4_virt =
        VirtAddr::new(hhdm.as_u64() + user_l4_frame.start_address().as_u64());
    let l4_table = unsafe { &mut *l4_virt.as_mut_ptr::<PageTable>() };
    let mut mapper =
        unsafe { OffsetPageTable::new(l4_table, VirtAddr::new(hhdm.as_u64())) };

    let memory = MEMORY.get().unwrap();
    let mut phys_mem = memory.physical_memory.lock();

    for (i, &frame) in frames.iter().enumerate() {
        let vaddr = VirtAddr::new(start_vaddr + i as u64 * Size4KiB::SIZE);
        let page: Page<Size4KiB> = Page::containing_address(vaddr);

        let mut pt_alloc = phys_mem.get_user_mode_frame_allocator();
        let result = unsafe { mapper.map_to(page, frame, page_flags, &mut pt_alloc) };
        drop(pt_alloc);

        if result.is_err() {
            // Rollback page table entries only — do NOT free the shared frames.
            rollback(&mut mapper, &mut phys_mem, start_vaddr, i as u64, false);
            user_vaddr::free_user_pages(
                &mut inner.user_vaddr_set,
                start_vaddr,
                n_pages * Size4KiB::SIZE,
            );
            return None;
        }
    }

    Some(start_vaddr)
}

/// Free the physical pages backing a shared buffer.
///
/// The caller must ensure all page-table mappings to these frames have been
/// removed (via `sys_munmap`) before calling this, otherwise the frames may
/// be reused while still virtually accessible.
pub fn destroy_shared_buf(id: SharedBufId) {
    let buf = SHARED_BUF_REGISTRY.lock().remove(&id);
    if let Some(buf) = buf {
        let memory = MEMORY.get().unwrap();
        let mut phys_mem = memory.physical_memory.lock();
        for frame in buf.frames {
            let _ = phys_mem.free_frame(frame, MemoryType::SharedBuffer);
        }
    }
}

/// Unmap `count` pages starting at `start_vaddr`.
/// If `free_frames` is true, also free each frame as `SharedBuffer`.
fn rollback(
    mapper: &mut OffsetPageTable,
    phys_mem: &mut crate::memory::physical_memory::PhysicalMemory,
    start_vaddr: u64,
    count: u64,
    free_frames: bool,
) {
    for j in 0..count {
        let vaddr = VirtAddr::new(start_vaddr + j * Size4KiB::SIZE);
        let page: Page<Size4KiB> = Page::containing_address(vaddr);
        if let Ok((frame, _, flush)) = mapper.unmap(page) {
            flush.flush();
            if free_frames {
                let _ = phys_mem.free_frame(frame, MemoryType::SharedBuffer);
            }
        }
    }
}
