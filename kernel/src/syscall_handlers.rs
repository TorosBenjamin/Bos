use crate::graphics::display::DISPLAY;
use crate::memory::MEMORY;
use crate::memory::cpu_local_data::get_local;
use crate::memory::physical_memory::MemoryType;
use crate::memory::user_vaddr;
use crate::task::task::{TaskKind, TaskState};
use core::sync::atomic::Ordering;
use embedded_graphics::Pixel;
use embedded_graphics::geometry::Point;
use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::primitives::Rectangle;
use ez_paging::{ConfigurableFlags, Owned4KibFrame, Page, PageSize};
use kernel_api_types::graphics::{GraphicsResult, PixelData, Rect, Rgb888Raw};
use kernel_api_types::{MMAP_EXEC, MMAP_WRITE};
use x86_64::registers::model_specific::PatMemoryType;
use x86_64::structures::paging::PhysFrame;
use x86_64::{PhysAddr, VirtAddr};

/// Syscall: draw multiple pixels from user-space
pub fn sys_draw_iter(pixels_ptr: u64, len: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    let pixels: &[PixelData] =
        unsafe { core::slice::from_raw_parts(pixels_ptr as *const PixelData, len as usize) };

    let pixels_iter = pixels.iter().map(|p| {
        let color = raw_to_rgb888(p.rgb_raw);
        Pixel(Point::new(p.x as i32, p.y as i32), color)
    });

    // Draw the pixels
    if DISPLAY.draw_iter(pixels_iter).is_err() {
        return GraphicsResult::InvalidInput as u64;
    }

    GraphicsResult::Ok as u64
}

/// Syscall: fill a solid rectangle
pub fn sys_fill_solid(rect_ptr: u64, rgb_raw: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    // SAFETY: rect_ptr comes from userspace, must be validated in real kernel
    // TODO: Pointer validation
    let rect = unsafe { &*(rect_ptr as *const Rect) };

    let color = raw_to_rgb888(rgb_raw as u32);

    let eg_rect = Rectangle::new(
        Point::new(rect.x as i32, rect.y as i32),
        embedded_graphics::geometry::Size::new(rect.width, rect.height),
    );

    if DISPLAY.fill_solid(&eg_rect, color).is_err() {
        return GraphicsResult::InvalidInput as u64;
    }

    GraphicsResult::Ok as u64
}

/// Syscall: return the bounding box of the framebuffer
pub fn sys_get_bounding_box(rect_out_ptr: u64, _: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    // TODO: Pointer validation
    let rect_out = unsafe { &mut *(rect_out_ptr as *mut Rect) };

    let bb = DISPLAY.bounding_box();

    rect_out.x = bb.top_left.x as u32;
    rect_out.y = bb.top_left.y as u32;
    rect_out.width = bb.size.width;
    rect_out.height = bb.size.height;

    GraphicsResult::Ok as u64
}

/// Exit the current task. Marks it as a zombie, enables interrupts, and halts.
/// The timer will fire and the scheduler will drop the zombie from the queue.
pub fn sys_exit() -> ! {
    let cpu = get_local();
    {
        let rq = cpu.run_queue.get().unwrap().lock();
        if let Some(current) = &rq.current_task {
            current.state.store(TaskState::Zombie, Ordering::Relaxed);
        }
    }
    // Enable interrupts and halt — timer will schedule another task.
    // The zombie won't be re-queued by the scheduler.
    x86_64::instructions::interrupts::enable();
    loop {
        x86_64::instructions::hlt();
    }
}

/// Syscall: read a key event (blocking).
///
/// The caller passes a pointer to a `KeyEvent` in `key_event_out_ptr`.
/// If a key is available, it's written immediately and we return 0.
/// If no key is available, we spin with `hlt` until the keyboard ISR delivers one.
pub fn sys_read_key(key_event_out_ptr: u64, _: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    let out = key_event_out_ptr as *mut kernel_api_types::KeyEvent;

    loop {
        if let Some(event) = crate::drivers::keyboard::try_read_key() {
            // Safety: pointer comes from userland, TODO: validate
            unsafe { core::ptr::write(out, event) };
            return 0;
        }
        // No key available — enable interrupts briefly and halt to wait for IRQ
        x86_64::instructions::interrupts::enable();
        x86_64::instructions::hlt();
        x86_64::instructions::interrupts::disable();
    }
}

/// Syscall: yield the current timeslice.
///
/// Enables interrupts and halts — the timer interrupt will immediately reschedule.
pub fn sys_yield(_: u64, _: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    x86_64::instructions::interrupts::enable();
    x86_64::instructions::hlt();
    x86_64::instructions::interrupts::disable();
    0
}

/// Syscall: spawn a new user task from ELF bytes in the caller's memory.
///
/// Arguments: elf_ptr, elf_len, child_arg
/// Returns: task ID on success, 0 on failure.
pub fn sys_spawn(elf_ptr: u64, elf_len: u64, child_arg: u64, _: u64, _: u64, _: u64) -> u64 {
    // Basic validation
    if elf_ptr == 0 || elf_len == 0 || elf_len > 64 * 1024 * 1024 {
        return 0;
    }

    // Verify caller is a user task
    let cpu = get_local();
    {
        let rq = cpu.run_queue.get().unwrap().lock();
        match &rq.current_task {
            Some(t) if t.kind == TaskKind::User => {}
            _ => return 0,
        }
    }

    // Build slice from user memory — caller's CR3 is still active
    let elf_bytes = unsafe {
        core::slice::from_raw_parts(elf_ptr as *const u8, elf_len as usize)
    };

    match crate::user_task_from_elf::create_user_task_from_elf_bytes(elf_bytes, child_arg) {
        Ok(task) => {
            let id = task.id.to_u64();
            crate::task::global_scheduler::spawn_task(task);
            id
        }
        Err(_) => 0,
    }
}

pub fn raw_to_rgb888(raw: Rgb888Raw) -> Rgb888 {
    let r = ((raw >> 16) & 0xFF) as u8;
    let g = ((raw >> 8) & 0xFF) as u8;
    let b = (raw & 0xFF) as u8;
    Rgb888::new(r, g, b)
}

/// Syscall: allocate virtual memory for the calling user task.
///
/// Arguments: size (bytes), flags (MMAP_WRITE | MMAP_EXEC)
/// Returns: start virtual address, or 0 on failure.
pub fn sys_mmap(size: u64, flags: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    if size == 0 {
        return 0;
    }

    let n_pages = size.div_ceil(4096);
    let page_size = PageSize::_4KiB;

    // Get the current task
    let cpu = get_local();
    let task = {
        let rq = cpu.run_queue.get().unwrap().lock();
        match &rq.current_task {
            Some(t) if t.kind == TaskKind::User => t.clone(),
            _ => return 0,
        }
    };

    let mut inner = task.inner.lock();

    // Find a gap in the user vaddr set
    let start_vaddr = match user_vaddr::allocate_user_pages(&mut inner.user_vaddr_set, n_pages) {
        Some(addr) => addr,
        None => return 0,
    };

    let configurable_flags = ConfigurableFlags {
        pat_memory_type: PatMemoryType::WriteBack,
        writable: (flags & MMAP_WRITE) != 0,
        executable: (flags & MMAP_EXEC) != 0,
    };

    let user_l4 = match &mut inner.user_page_table {
        Some(pt) => pt,
        None => {
            // Roll back vaddr allocation
            user_vaddr::free_user_pages(&mut inner.user_vaddr_set, start_vaddr, n_pages * 4096);
            return 0;
        }
    };

    let memory = MEMORY.get().unwrap();
    let mut physical_memory = memory.physical_memory.lock();

    // Allocate and map each page
    for i in 0..n_pages {
        let vaddr = start_vaddr + i * 4096;
        let page = Page::new(VirtAddr::new(vaddr), page_size).unwrap();

        let frame = match physical_memory.allocate_frame_with_type(page_size, MemoryType::UsedByUserMode) {
            Some(f) => f,
            None => {
                // Rollback: unmap and free already-mapped pages
                for j in 0..i {
                    let rollback_vaddr = start_vaddr + j * 4096;
                    let rollback_page = Page::new(VirtAddr::new(rollback_vaddr), page_size).unwrap();
                    if let Ok(unmapped_frame) = unsafe { user_l4.unmap_page(rollback_page) } {
                        let phys_frame = PhysFrame::from_start_address(unmapped_frame.start_addr()).unwrap();
                        let owned = unsafe { Owned4KibFrame::new(phys_frame) };
                        let _ = physical_memory.free_frame(owned, MemoryType::UsedByUserMode);
                    }
                }
                user_vaddr::free_user_pages(&mut inner.user_vaddr_set, start_vaddr, n_pages * 4096);
                return 0;
            }
        };

        // Zero the frame for security
        let frame_virt = PhysAddr::new(frame.start_addr().as_u64());
        let frame_ptr = crate::memory::physical_memory::OffsetMappedPhysAddr::offset_mapped(frame_virt);
        unsafe {
            core::ptr::write_bytes(frame_ptr.as_mut_ptr::<u8>(), 0, 4096);
        }

        let mut frame_allocator = physical_memory.get_user_mode_frame_allocator();
        if unsafe { user_l4.map_page(page, frame, configurable_flags, &mut frame_allocator) }.is_err() {
            // Free the frame we just allocated but couldn't map
            let phys_frame = PhysFrame::from_start_address(frame.start_addr()).unwrap();
            let owned = unsafe { Owned4KibFrame::new(phys_frame) };
            drop(frame_allocator);
            let _ = physical_memory.free_frame(owned, MemoryType::UsedByUserMode);
            // Rollback previously mapped pages
            for j in 0..i {
                let rollback_vaddr = start_vaddr + j * 4096;
                let rollback_page = Page::new(VirtAddr::new(rollback_vaddr), page_size).unwrap();
                if let Ok(unmapped_frame) = unsafe { user_l4.unmap_page(rollback_page) } {
                    let phys_frame = PhysFrame::from_start_address(unmapped_frame.start_addr()).unwrap();
                    let owned = unsafe { Owned4KibFrame::new(phys_frame) };
                    let _ = physical_memory.free_frame(owned, MemoryType::UsedByUserMode);
                }
            }
            user_vaddr::free_user_pages(&mut inner.user_vaddr_set, start_vaddr, n_pages * 4096);
            return 0;
        }
    }

    start_vaddr
}

/// Syscall: unmap virtual memory from the calling user task.
///
/// Arguments: addr (page-aligned), size (bytes)
/// Returns: 0 on success, !0 on failure.
pub fn sys_munmap(addr: u64, size: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    if size == 0 || addr % 4096 != 0 {
        return !0u64;
    }

    let n_pages = size.div_ceil(4096);
    let page_size = PageSize::_4KiB;

    // Get the current task
    let cpu = get_local();
    let task = {
        let rq = cpu.run_queue.get().unwrap().lock();
        match &rq.current_task {
            Some(t) if t.kind == TaskKind::User => t.clone(),
            _ => return !0u64,
        }
    };

    let mut inner = task.inner.lock();

    // Verify and remove the range from user_vaddr_set
    let total_size = n_pages * 4096;
    if !user_vaddr::free_user_pages(&mut inner.user_vaddr_set, addr, total_size) {
        return !0u64;
    }

    let user_l4 = match &mut inner.user_page_table {
        Some(pt) => pt,
        None => return !0u64,
    };

    let memory = MEMORY.get().unwrap();
    let mut physical_memory = memory.physical_memory.lock();

    // Unmap each page and free its frame
    for i in 0..n_pages {
        let vaddr = addr + i * 4096;
        let page = Page::new(VirtAddr::new(vaddr), page_size).unwrap();

        if let Ok(unmapped_frame) = unsafe { user_l4.unmap_page(page) } {
            let phys_frame = PhysFrame::from_start_address(unmapped_frame.start_addr()).unwrap();
            let owned = unsafe { Owned4KibFrame::new(phys_frame) };
            let _ = physical_memory.free_frame(owned, MemoryType::UsedByUserMode);
        }
    }

    0
}
