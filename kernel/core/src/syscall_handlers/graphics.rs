use crate::graphics::display::{DISPLAY, DISPLAY_OWNER};
use crate::memory::MEMORY;
use crate::memory::hhdm_offset::hhdm_offset;
use crate::task::global_scheduler::TASK_TABLE;
use crate::task::task::TaskId;
use core::sync::atomic::Ordering;
use kernel_api_types::graphics::{DisplayInfo, GraphicsResult, Rect, FRAMEBUFFER_USER_VADDR};
use nodit::interval::ii;
use x86_64::structures::paging::{
    Mapper, OffsetPageTable, Page, PageSize, PageTable, PageTableFlags, PhysFrame, Size4KiB,
};
use x86_64::{PhysAddr, VirtAddr};
use super::validate_user_ptr;

/// Syscall: return the bounding box of the framebuffer.
pub fn sys_get_bounding_box(rect_out_ptr: u64, _: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    if !crate::graphics::display::is_display_owner() {
        return GraphicsResult::PermissionDenied as u64;
    }
    if !validate_user_ptr(rect_out_ptr, core::mem::size_of::<Rect>() as u64) {
        return GraphicsResult::InvalidInput as u64;
    }

    let rect_out = unsafe { &mut *(rect_out_ptr as *mut Rect) };
    let bb = DISPLAY.bounding_box();
    rect_out.x = bb.top_left.x as u32;
    rect_out.y = bb.top_left.y as u32;
    rect_out.width = bb.size.width;
    rect_out.height = bb.size.height;

    GraphicsResult::Ok as u64
}

/// Syscall: get display info (dimensions and pixel format).
///
/// Arguments: info_out_ptr
/// Returns: GraphicsResult code.
pub fn sys_get_display_info(info_out_ptr: u64, _: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    if !validate_user_ptr(info_out_ptr, core::mem::size_of::<DisplayInfo>() as u64) {
        return GraphicsResult::InvalidInput as u64;
    }

    let info = DISPLAY.get_display_info();
    unsafe { core::ptr::write(info_out_ptr as *mut DisplayInfo, info) };

    GraphicsResult::Ok as u64
}

/// Syscall: transfer display ownership to another task.
///
/// Arguments: new_owner_task_id
/// Returns: 0 on success, 1 if caller is not the current owner,
///          2 if target task not found, 3 if mapping failed.
pub fn sys_transfer_display(new_owner_id: u64, _: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    if !crate::graphics::display::is_display_owner() {
        return 1;
    }

    let target_task = {
        let table = TASK_TABLE.lock();
        match table.get(&TaskId::from_u64(new_owner_id)) {
            Some(task) => task.clone(),
            None => return 2,
        }
    };

    let (fb_phys_addr, fb_size) = DISPLAY.get_fb_phys_and_size();
    let user_fb_virt = VirtAddr::new(FRAMEBUFFER_USER_VADDR);

    let mut task_inner = target_task.inner.lock();

    // Mark virtual address range as used in the task's vaddr set
    let page_count = fb_size.div_ceil(Size4KiB::SIZE);
    let virt_start = user_fb_virt.as_u64();
    let virt_end = virt_start + (page_count * Size4KiB::SIZE) - 1;
    let _ = task_inner.user_vaddr_set.insert_merge_touching(ii(virt_start, virt_end));

    let hhdm = hhdm_offset();
    let user_l4_frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(target_task.cr3));
    let l4_virt_addr = VirtAddr::new(hhdm.as_u64() + user_l4_frame.start_address().as_u64());
    let l4_table = unsafe { &mut *l4_virt_addr.as_mut_ptr::<PageTable>() };
    let mut mapper = unsafe { OffsetPageTable::new(l4_table, VirtAddr::new(hhdm.as_u64())) };

    let memory_system = MEMORY.get().unwrap();
    let mut physical_memory = memory_system.physical_memory.lock();
    let mut frame_allocator = physical_memory.get_user_mode_frame_allocator();

    let flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::USER_ACCESSIBLE
        | PageTableFlags::WRITE_THROUGH;

    log::info!("TransferDisplay: mapping {} pages starting at virt={:#x}, phys={:#x}",
        page_count, user_fb_virt.as_u64(), fb_phys_addr.as_u64());

    for i in 0..page_count {
        let offset = i * Size4KiB::SIZE;
        let page = Page::<Size4KiB>::containing_address(user_fb_virt + offset);
        let frame = PhysFrame::<Size4KiB>::containing_address(fb_phys_addr + offset);

        unsafe {
            if let Ok(mapping) = mapper.map_to(page, frame, flags, &mut frame_allocator) {
                mapping.ignore();
            } else {
                log::error!("TransferDisplay: map_to failed at page {}", i);
                return 3;
            }
        }
    }
    log::info!("TransferDisplay: mapping complete");

    DISPLAY_OWNER.store(new_owner_id, Ordering::SeqCst);
    0
}
