use crate::graphics::display::DISPLAY;
use crate::limine_requests::MODULE_REQUEST;
use crate::memory::MEMORY;
use crate::memory::cpu_local_data::get_local;
use crate::memory::physical_memory::MemoryType;
use crate::memory::user_vaddr;
use crate::memory::page_tables::get_kernel_vaddr_from_user_vaddr;
use crate::task::task::{TaskKind, TaskState};
use core::sync::atomic::Ordering;
use ez_paging::{ConfigurableFlags, Owned4KibFrame, Page, PageSize};
use kernel_api_types::graphics::{DisplayInfo, GraphicsResult, Rect};
use kernel_api_types::{MMAP_EXEC, MMAP_WRITE};
use x86_64::registers::model_specific::PatMemoryType;
use x86_64::structures::paging::PhysFrame;
use x86_64::{PhysAddr, VirtAddr};

/// Syscall: return the bounding box of the framebuffer
pub fn sys_get_bounding_box(rect_out_ptr: u64, _: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    if !crate::graphics::display::is_display_owner() {
        return GraphicsResult::PermissionDenied as u64;
    }

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

/// Syscall: create a new IPC channel.
///
/// Arguments: send_ep_out_ptr, recv_ep_out_ptr, capacity
/// Writes the two endpoint IDs to the output pointers.
/// Returns: IPC status code.
pub fn sys_channel_create(send_ep_out_ptr: u64, recv_ep_out_ptr: u64, capacity: u64, _: u64, _: u64, _: u64) -> u64 {
    if send_ep_out_ptr == 0 || recv_ep_out_ptr == 0 {
        return kernel_api_types::IPC_ERR_INVALID_ARGS;
    }

    let cap = if capacity == 0 {
        crate::ipc::DEFAULT_CHANNEL_CAPACITY
    } else {
        (capacity as usize).clamp(1, crate::ipc::MAX_CHANNEL_CAPACITY)
    };

    let (send_id, recv_id) = crate::ipc::create_channel(cap);

    unsafe {
        core::ptr::write(send_ep_out_ptr as *mut u64, send_id);
        core::ptr::write(recv_ep_out_ptr as *mut u64, recv_id);
    }

    kernel_api_types::IPC_OK
}

/// Syscall: send a message on a channel endpoint.
///
/// Arguments: endpoint_id, msg_ptr, msg_len
/// Returns: IPC status code.
pub fn sys_channel_send(endpoint_id: u64, msg_ptr: u64, msg_len: u64, _: u64, _: u64, _: u64) -> u64 {
    if msg_len > crate::ipc::MAX_MESSAGE_SIZE as u64 {
        return kernel_api_types::IPC_ERR_MSG_TOO_LARGE;
    }
    if msg_len > 0 && msg_ptr == 0 {
        return kernel_api_types::IPC_ERR_INVALID_ARGS;
    }

    let data = if msg_len > 0 {
        unsafe { core::slice::from_raw_parts(msg_ptr as *const u8, msg_len as usize) }
    } else {
        &[]
    };

    loop {
        match crate::ipc::try_send(endpoint_id, data) {
            Ok(()) => return kernel_api_types::IPC_OK,
            Err(crate::ipc::IpcError::ChannelFull) => {
                // Spin-yield: let other tasks run
                x86_64::instructions::interrupts::enable();
                x86_64::instructions::hlt();
                x86_64::instructions::interrupts::disable();
            }
            Err(e) => return ipc_error_to_code(e),
        }
    }
}

/// Syscall: receive a message from a channel endpoint.
///
/// Arguments: endpoint_id, buf_ptr, buf_cap, bytes_read_out_ptr
/// Returns: IPC status code.
pub fn sys_channel_recv(endpoint_id: u64, buf_ptr: u64, buf_cap: u64, bytes_read_out_ptr: u64, _: u64, _: u64) -> u64 {
    if buf_ptr == 0 || bytes_read_out_ptr == 0 {
        return kernel_api_types::IPC_ERR_INVALID_ARGS;
    }

    loop {
        match crate::ipc::try_recv(endpoint_id) {
            Ok(msg) => {
                let copy_len = msg.len().min(buf_cap as usize);
                unsafe {
                    core::ptr::copy_nonoverlapping(msg.as_ptr(), buf_ptr as *mut u8, copy_len);
                    core::ptr::write(bytes_read_out_ptr as *mut u64, copy_len as u64);
                }
                return kernel_api_types::IPC_OK;
            }
            Err(crate::ipc::IpcError::WouldBlock) => {
                // Spin-yield: let other tasks run
                x86_64::instructions::interrupts::enable();
                x86_64::instructions::hlt();
                x86_64::instructions::interrupts::disable();
            }
            Err(e) => return ipc_error_to_code(e),
        }
    }
}

/// Syscall: close a channel endpoint.
///
/// Arguments: endpoint_id
/// Returns: IPC status code.
pub fn sys_channel_close(endpoint_id: u64, _: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    match crate::ipc::close_endpoint(endpoint_id) {
        Ok(()) => kernel_api_types::IPC_OK,
        Err(e) => ipc_error_to_code(e),
    }
}

fn ipc_error_to_code(e: crate::ipc::IpcError) -> u64 {
    match e {
        crate::ipc::IpcError::InvalidEndpoint => kernel_api_types::IPC_ERR_INVALID_ENDPOINT,
        crate::ipc::IpcError::WrongDirection => kernel_api_types::IPC_ERR_WRONG_DIRECTION,
        crate::ipc::IpcError::PeerClosed => kernel_api_types::IPC_ERR_PEER_CLOSED,
        crate::ipc::IpcError::ChannelFull => kernel_api_types::IPC_ERR_CHANNEL_FULL,
        crate::ipc::IpcError::WouldBlock => kernel_api_types::IPC_ERR_CHANNEL_FULL, // shouldn't surface
        crate::ipc::IpcError::MessageTooLarge => kernel_api_types::IPC_ERR_MSG_TOO_LARGE,
        crate::ipc::IpcError::InvalidArgs => kernel_api_types::IPC_ERR_INVALID_ARGS,
    }
}

/// Syscall: transfer display ownership to another task.
///
/// Arguments: new_owner_task_id
/// Returns: 0 on success, 1 if caller is not the current owner,
///          2 if target task not found.
pub fn sys_transfer_display(new_owner_id: u64, _: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    use crate::graphics::display::DISPLAY_OWNER;
    use crate::task::global_scheduler::TASK_TABLE;
    use crate::task::task::TaskId;
    use core::sync::atomic::Ordering;

    if !crate::graphics::display::is_display_owner() {
        return 1;
    }

    {
        let table = TASK_TABLE.lock();
        if !table.contains_key(&TaskId::from_u64(new_owner_id)) {
            return 2;
        }
    }

    DISPLAY_OWNER.store(new_owner_id, Ordering::Relaxed);
    0
}

/// Syscall: load a Limine boot module by name.
///
/// Arguments: name_ptr, name_len, buf_ptr, buf_cap
///
/// Size query: if buf_ptr == 0 && buf_cap == 0, returns the module size (or 0 if not found).
/// Copy: copies module bytes to buf, returns bytes written (or 0 on failure).
pub fn sys_get_module(name_ptr: u64, name_len: u64, buf_ptr: u64, buf_cap: u64, _: u64, _: u64) -> u64 {
    if name_ptr == 0 || name_len == 0 || name_len > 256 {
        return 0;
    }

    let name_bytes = unsafe { core::slice::from_raw_parts(name_ptr as *const u8, name_len as usize) };
    let name = match core::str::from_utf8(name_bytes) {
        Ok(s) => s,
        Err(_) => return 0,
    };

    // Build path by prepending "/" to name
    let mut path_buf = [0u8; 258];
    path_buf[0] = b'/';
    path_buf[1..1 + name.len()].copy_from_slice(name.as_bytes());
    let path = &path_buf[..1 + name.len()];

    let response = match MODULE_REQUEST.get_response() {
        Some(r) => r,
        None => return 0,
    };

    let module = match response.modules().iter().find(|m| m.path().to_bytes() == path) {
        Some(m) => m,
        None => return 0,
    };

    let module_size = module.size();

    // Size query mode
    if buf_ptr == 0 && buf_cap == 0 {
        return module_size;
    }

    // Buffer too small
    if buf_cap < module_size {
        return 0;
    }

    // Copy module bytes to user buffer
    unsafe {
        core::ptr::copy_nonoverlapping(
            module.addr() as *const u8,
            buf_ptr as *mut u8,
            module_size as usize,
        );
    }

    module_size
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

/// Syscall: present a dirty rectangle from a user-space pixel buffer to the framebuffer.
///
/// Arguments: buf_ptr, buf_width, dirty_x, dirty_y, dirty_w, dirty_h
/// Returns: GraphicsResult code.
pub fn sys_present_display(buf_ptr: u64, buf_width: u64, dirty_x: u64, dirty_y: u64, dirty_w: u64, dirty_h: u64) -> u64 {
    if !crate::graphics::display::is_display_owner() {
        return GraphicsResult::PermissionDenied as u64;
    }

    if buf_ptr == 0 || buf_width == 0 || dirty_w == 0 || dirty_h == 0 {
        return GraphicsResult::Ok as u64;
    }

    // Get framebuffer dimensions for bounds checking
    let bb = DISPLAY.bounding_box();
    let fb_w = bb.size.width as u64;
    let fb_h = bb.size.height as u64;

    // Clamp dirty rect to framebuffer bounds
    if dirty_x >= fb_w || dirty_y >= fb_h {
        return GraphicsResult::Ok as u64;
    }
    let clamped_w = dirty_w.min(fb_w - dirty_x) as usize;
    let clamped_h = dirty_h.min(fb_h - dirty_y) as usize;

    let cpu = get_local();
    let task = {
        let rq = cpu.run_queue.get().unwrap().lock();
        match &rq.current_task {
            Some(t) if t.kind == TaskKind::User => t.clone(),
            _ => return GraphicsResult::PermissionDenied as u64,
        }
    };

    let mut task_inner = task.inner.lock();
    let user_vaddr_set = &task_inner.user_vaddr_set;

    let start_vaddr = VirtAddr::new(buf_ptr);

    let end_vaddr_val = if clamped_h > 0 && clamped_w > 0 {
        let max_row_offset = (dirty_y + clamped_h as u64 - 1) * buf_width;
        let max_col_offset = dirty_x + clamped_w as u64;
        buf_ptr + (max_row_offset + max_col_offset) * core::mem::size_of::<u32>() as u64
    } else {
        buf_ptr + core::mem::size_of::<u32>() as u64
    };
    let end_vaddr = VirtAddr::new(end_vaddr_val);

    if !user_vaddr::is_user_vaddr_valid_range(user_vaddr_set, start_vaddr, end_vaddr) {
        return GraphicsResult::InvalidInput as u64;
    }

    let user_l4 = match &mut task_inner.user_page_table {
        Some(pt) => pt,
        None => return GraphicsResult::PermissionDenied as u64,
    };

    let kernel_buf_ptr = match get_kernel_vaddr_from_user_vaddr(user_l4, start_vaddr) {
        Some(vaddr) => vaddr.as_u64() as *const u32,
        None => return GraphicsResult::InvalidInput as u64,
    };

    unsafe {
        DISPLAY.copy_rect_from_user(
            kernel_buf_ptr,
            buf_width as usize,
            dirty_x as usize,
            dirty_y as usize,
            clamped_w,
            clamped_h,
        );
    }

    GraphicsResult::Ok as u64
}

/// Syscall: get display info (dimensions and pixel format).
///
/// Arguments: info_out_ptr
/// Returns: GraphicsResult code.
pub fn sys_get_display_info(info_out_ptr: u64, _: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    if info_out_ptr == 0 {
        return GraphicsResult::InvalidInput as u64;
    }

    let info = DISPLAY.get_display_info();

    // TODO: Pointer validation
    unsafe {
        core::ptr::write(info_out_ptr as *mut DisplayInfo, info);
    }

    GraphicsResult::Ok as u64
}
