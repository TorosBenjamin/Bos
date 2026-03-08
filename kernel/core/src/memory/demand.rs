use alloc::sync::Arc;
use x86_64::structures::paging::mapper::MapToError;
use x86_64::structures::paging::{
    Mapper, OffsetPageTable, Page, PageSize, PageTable, PhysFrame, Size4KiB, Translate,
};
use x86_64::{PhysAddr, VirtAddr};

use crate::memory::cpu_local_data::get_local;
use crate::memory::hhdm_offset::hhdm_offset;
use crate::memory::physical_memory::{MemoryType, OffsetMappedPhysAddr};
use crate::memory::user_vaddr;
use crate::memory::MEMORY;
use crate::task::task::{Task, VmaBacking};

/// Called from the page-fault handler (after swapgs) for not-present user faults.
///
/// Checks if the faulting address belongs to an `Anonymous` VMA, and if so:
/// allocates a zeroed physical frame, installs the PTE, and returns `true` so
/// the fault handler can iretq to retry the instruction.
///
/// Returns `false` if the address is not in an `Anonymous` VMA, on OOM, or on
/// any mapping error — the caller should kill the task.
pub fn try_demand_fill(faulting_addr: u64) -> bool {
    let page_addr = faulting_addr & !(Size4KiB::SIZE - 1); // align down to 4 KiB

    // Get the current task. Interrupts are disabled here (we're in an ISR).
    let cpu = get_local();
    let task = {
        let rq = cpu.run_queue.get().unwrap().lock();
        rq.current_task.clone()
    };
    let task = match task {
        Some(t) => t,
        None => return false,
    };

    // Look up the VMA; only Anonymous VMAs are demand-filled.
    let entry = {
        let inner = task.inner.lock();
        match user_vaddr::lookup_vma(&inner.user_vmas, page_addr) {
            Some(e) if e.backing == VmaBacking::Anonymous => e,
            _ => return false,
        }
    };

    // Set up the mapper for this task's address space.
    let hhdm_off = hhdm_offset();
    let user_l4_frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(task.cr3));
    let l4_virt = VirtAddr::new(hhdm_off.as_u64() + user_l4_frame.start_address().as_u64());
    let l4_table = unsafe { &mut *l4_virt.as_mut_ptr::<PageTable>() };
    let mut mapper = unsafe { OffsetPageTable::new(l4_table, VirtAddr::new(hhdm_off.as_u64())) };

    // Allocate a zeroed frame and map it.
    let memory = MEMORY.get().unwrap();
    let mut phys_mem = memory.physical_memory.lock();

    let frame = match phys_mem.allocate_frame_with_type(MemoryType::UsedByUserMode) {
        Some(f) => f,
        None => return false,
    };

    // Security: zero the frame before exposing it to user space.
    unsafe {
        core::ptr::write_bytes(
            frame.start_address().offset_mapped().as_mut_ptr::<u8>(),
            0,
            Size4KiB::SIZE as usize,
        );
    }

    let page = Page::<Size4KiB>::containing_address(VirtAddr::new(page_addr));
    let mut frame_allocator = phys_mem.get_user_mode_frame_allocator();
    let map_result = unsafe { mapper.map_to(page, frame, entry.flags, &mut frame_allocator) };
    drop(frame_allocator);

    match map_result {
        Ok(flush) => {
            flush.flush();
            true
        }
        Err(MapToError::PageAlreadyMapped(_)) => {
            // Race: another CPU already filled this page — still success.
            let _ = phys_mem.free_frame(frame, MemoryType::UsedByUserMode);
            true
        }
        Err(_) => {
            let _ = phys_mem.free_frame(frame, MemoryType::UsedByUserMode);
            false
        }
    }
}

/// Ensure every page in `[start, end)` is present for the given task.
///
/// Called from `validate_user_ptr` before the kernel reads/writes user memory
/// in a syscall handler. For each absent page in an `Anonymous` VMA, allocates
/// a zeroed frame and installs the PTE.
///
/// Returns `true` if all pages are (or become) present; `false` if any page is
/// outside a VMA, in a VMA with wrong backing, or on OOM/mapping failure.
pub fn prefault_user_range(task: &Arc<Task>, start: u64, end: u64) -> bool {
    if start >= end {
        return true;
    }

    let hhdm_off = hhdm_offset();
    let user_l4_frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(task.cr3));
    let l4_virt = VirtAddr::new(hhdm_off.as_u64() + user_l4_frame.start_address().as_u64());
    let l4_table = unsafe { &mut *l4_virt.as_mut_ptr::<PageTable>() };
    let mut mapper = unsafe { OffsetPageTable::new(l4_table, VirtAddr::new(hhdm_off.as_u64())) };

    let mut addr = start & !(Size4KiB::SIZE - 1); // align down to first page

    while addr < end {
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(addr));

        // If already mapped, skip.
        if mapper.translate_addr(VirtAddr::new(addr)).is_some() {
            addr += Size4KiB::SIZE;
            continue;
        }

        // Look up the VMA; only Anonymous VMAs are demand-filled.
        let vma = {
            let inner = task.inner.lock();
            user_vaddr::lookup_vma(&inner.user_vmas, addr)
        };
        let vma = match vma {
            Some(v) if v.backing == VmaBacking::Anonymous => v,
            _ => return false,
        };

        // Allocate and install a zeroed frame.
        let memory = MEMORY.get().unwrap();
        let mut phys_mem = memory.physical_memory.lock();

        let frame = match phys_mem.allocate_frame_with_type(MemoryType::UsedByUserMode) {
            Some(f) => f,
            None => return false,
        };

        unsafe {
            core::ptr::write_bytes(
                frame.start_address().offset_mapped().as_mut_ptr::<u8>(),
                0,
                Size4KiB::SIZE as usize,
            );
        }

        let mut frame_allocator = phys_mem.get_user_mode_frame_allocator();
        let map_result = unsafe { mapper.map_to(page, frame, vma.flags, &mut frame_allocator) };
        drop(frame_allocator);

        match map_result {
            Ok(flush) => {
                flush.flush();
            }
            Err(MapToError::PageAlreadyMapped(_)) => {
                // Race — another CPU already mapped this page.
                let _ = phys_mem.free_frame(frame, MemoryType::UsedByUserMode);
            }
            Err(_) => {
                let _ = phys_mem.free_frame(frame, MemoryType::UsedByUserMode);
                return false;
            }
        }

        addr += Size4KiB::SIZE;
    }

    true
}
