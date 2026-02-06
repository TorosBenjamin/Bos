use crate::graphics::display::{DISPLAY, DISPLAY_OWNER};
use crate::limine_requests::MODULE_REQUEST;
use crate::memory::MEMORY;
use crate::memory::cpu_local_data::get_local;
use crate::memory::physical_memory::{MemoryType, OffsetMappedPhysAddr, PhysicalMemory};
use crate::memory::user_vaddr;
use crate::memory::page_tables::get_kernel_vaddr_from_user_vaddr;
use crate::task::task::{Task, TaskId, TaskKind, TaskState};
use core::sync::atomic::Ordering;
use nodit::interval::ii;
use kernel_api_types::graphics::{DisplayInfo, GraphicsResult, Rect, FRAMEBUFFER_USER_VADDR};
use kernel_api_types::{MMAP_EXEC, MMAP_WRITE};
use x86_64::structures::paging::{Mapper, OffsetPageTable, Page, PageSize, PageTable, PageTableFlags, PhysFrame, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};
use crate::memory::hhdm_offset::hhdm_offset;
use crate::task::global_scheduler::TASK_TABLE;

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
/// If no key is available, we spin-wait until the keyboard ISR delivers one.
pub fn sys_read_key(key_event_out_ptr: u64, _: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    let out = key_event_out_ptr as *mut kernel_api_types::KeyEvent;

    loop {
        if let Some(event) = crate::drivers::keyboard::try_read_key() {
            // Safety: pointer comes from userland, TODO: validate
            unsafe { core::ptr::write(out, event) };
            return 0;
        }
        // Spin-wait: we can't enable interrupts here because we're on
        // the syscall handler stack, not the task's kernel stack.
        core::hint::spin_loop();
    }
}

/// Syscall: yield the current timeslice.
///
/// Enables interrupts and halts — the timer interrupt will immediately reschedule.
/// When the timer preempts us here, it sees in_syscall=1 and uses the CpuContext
/// that was saved at syscall entry. We set rax in CpuContext to the return value
/// so the user task sees the correct result when it resumes via iretq.
pub fn sys_yield(_: u64, _: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    let ctx_ptr = get_local().current_context_ptr.load(Ordering::Relaxed);
    if !ctx_ptr.is_null() {
        unsafe { (*ctx_ptr).rax = 0; } // success return value
    }
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

    let n_pages = size.div_ceil(Size4KiB::SIZE);

    // Get current task
    let cpu = get_local();
    let task = {
        let rq = cpu.run_queue.get().unwrap().lock();
        match &rq.current_task {
            Some(t) if t.kind == TaskKind::User => t.clone(),
            _ => return 0,
        }
    };

    let mut inner = task.inner.lock();

    // Virtual Allocation
    let start_vaddr = match user_vaddr::allocate_user_pages(&mut inner.user_vaddr_set, n_pages) {
        Some(addr) => addr,
        None => return 0,
    };

    // Flags Setup
    // All mmaped pages need PRESENT and USER_ACCESSIBLE.
    let mut page_flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
    if (flags & MMAP_WRITE) != 0 {
        page_flags |= PageTableFlags::WRITABLE;
    }
    if (flags & MMAP_EXEC) == 0 {
        page_flags |= PageTableFlags::NO_EXECUTE;
    }

    // Setup the Mapper using the task's cr3
    let hhdm_offset = hhdm_offset();

    // Convert the task's cr3 (u64) into a PhysFrame
    let user_l4_frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(task.cr3));

    // Calculate the virtual address of the L4 table in the HHDM
    let l4_virt_addr = VirtAddr::new(hhdm_offset.as_u64() + user_l4_frame.start_address().as_u64());
    let l4_table = unsafe { &mut *l4_virt_addr.as_mut_ptr::<PageTable>() };

    // Create our standard x86_64 mapper
    let mut mapper = unsafe {
        OffsetPageTable::new(l4_table, VirtAddr::new(hhdm_offset.as_u64()))
    };

    let memory = MEMORY.get().unwrap();
    let mut physical_memory = memory.physical_memory.lock();

    // Allocation and Mapping Loop
    for i in 0..n_pages {
        let vaddr = VirtAddr::new(start_vaddr + i * Size4KiB::SIZE);
        let page: Page<Size4KiB> = Page::containing_address(vaddr);

        // Allocate physical frame
        let frame = match physical_memory.allocate_frame_with_type(MemoryType::UsedByUserMode) {
            Some(f) => f,
            None => {
                rollback_mmap(&mut mapper, &mut physical_memory, start_vaddr, i);
                user_vaddr::free_user_pages(&mut inner.user_vaddr_set, start_vaddr, n_pages * Size4KiB::SIZE);
                return 0;
            }
        };

        // Security: Zero the frame
        let frame_virt = frame.start_address().offset_mapped();
        unsafe {
            core::ptr::write_bytes(frame_virt.as_mut_ptr::<u8>(), 0, Size4KiB::SIZE as usize);
        }

        // Map it
        let mut frame_allocator = physical_memory.get_user_mode_frame_allocator();
        let map_result = unsafe {
            mapper.map_to(page, frame, page_flags, &mut frame_allocator)
        };
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

/// Helper to clean up partial mappings on failure
fn rollback_mmap(
    mapper: &mut OffsetPageTable,
    physical_memory: &mut PhysicalMemory,
    start_vaddr: u64,
    count: u64
) {
    for j in 0..count {
        let rollback_vaddr = VirtAddr::new(start_vaddr + j * Size4KiB::SIZE);
        let rollback_page: Page<Size4KiB> = Page::containing_address(rollback_vaddr);

        if let Ok((frame, _, flush)) = unsafe { mapper.unmap(rollback_page) } {
            flush.flush();
            let _ = physical_memory.free_frame(frame, MemoryType::UsedByUserMode);
        }
    }
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
                // Spin-wait: we can't enable interrupts here because we're on
                // the syscall handler stack, not the task's kernel stack.
                // If a timer interrupt fires, the scheduler would save RSP pointing
                // to this shared stack, corrupting state.
                core::hint::spin_loop();
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
                // We can safely enable interrupts here because the syscall entry
                // saved the user's full register state to CpuContext and set
                // in_syscall=1. The timer handler will skip re-saving and use
                // the already-saved user state.
                let ctx_ptr = get_local().current_context_ptr.load(Ordering::Relaxed);
                if !ctx_ptr.is_null() {
                    unsafe { (*ctx_ptr).rax = kernel_api_types::IPC_ERR_CHANNEL_FULL; }
                }
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
///          2 if target task not found, 3 if mapping failed.
pub fn sys_transfer_display(new_owner_id: u64, _: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    // 1. Permission Check
    if !crate::graphics::display::is_display_owner() {
        return 1;
    }

    // 2. Retrieve the target task
    let target_task = {
        let table = TASK_TABLE.lock();
        match table.get(&TaskId::from_u64(new_owner_id)) {
            Some(task) => task.clone(),
            None => return 2,
        }
    };

    // 3. Get framebuffer physical address and size from Display
    let (fb_phys_addr, fb_size) = DISPLAY.get_fb_phys_and_size();
    let user_fb_virt = VirtAddr::new(FRAMEBUFFER_USER_VADDR);

    // 4. Lock the task's internals
    let mut task_inner = target_task.inner.lock();

    // Mark virtual address range as used in the task's vaddr set
    let page_count = fb_size.div_ceil(Size4KiB::SIZE);
    let virt_start = user_fb_virt.as_u64();
    let virt_end = virt_start + (page_count * Size4KiB::SIZE) - 1;
    let _ = task_inner.user_vaddr_set.insert_merge_touching(ii(virt_start, virt_end));

    // 5. Setup the mapper using the target task's page table
    let hhdm = hhdm_offset();
    let user_l4_frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(target_task.cr3));
    let l4_virt_addr = VirtAddr::new(hhdm.as_u64() + user_l4_frame.start_address().as_u64());
    let l4_table = unsafe { &mut *l4_virt_addr.as_mut_ptr::<PageTable>() };
    let mut mapper = unsafe { OffsetPageTable::new(l4_table, VirtAddr::new(hhdm.as_u64())) };

    // 6. Get frame allocator for creating page table entries
    let memory_system = MEMORY.get().unwrap();
    let mut physical_memory = memory_system.physical_memory.lock();
    let mut frame_allocator = physical_memory.get_user_mode_frame_allocator();

    let flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::USER_ACCESSIBLE
        | PageTableFlags::WRITE_THROUGH; // Important for video memory!

    // 7. Map each framebuffer page into user space
    log::info!("TransferDisplay: mapping {} pages starting at virt={:#x}, phys={:#x}",
        page_count, user_fb_virt.as_u64(), fb_phys_addr.as_u64());
    for i in 0..page_count {
        let offset = i * Size4KiB::SIZE;
        let page = Page::<Size4KiB>::containing_address(user_fb_virt + offset);
        let frame = PhysFrame::<Size4KiB>::containing_address(fb_phys_addr + offset);

        unsafe {
            if let Ok(mapping) = mapper.map_to(page, frame, flags, &mut frame_allocator) {
                // Don't flush - we're mapping into target task's page table, not current CR3
                mapping.ignore();
            } else {
                log::error!("TransferDisplay: map_to failed at page {}", i);
                return 3; // Mapping failed (e.g., OOM for page tables)
            }
        }
    }
    log::info!("TransferDisplay: mapping complete");

    // 8. Update display ownership
    DISPLAY_OWNER.store(new_owner_id, Ordering::SeqCst);
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

pub fn sys_munmap(addr: u64, size: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    // 1. Validate alignment and size
    if size == 0 || addr % Size4KiB::SIZE != 0 {
        return !0u64;
    }

    let n_pages = size.div_ceil(Size4KiB::SIZE);

    // 2. Get the current task
    let cpu = get_local();
    let task = {
        let rq = cpu.run_queue.get().unwrap().lock();
        match &rq.current_task {
            Some(t) if t.kind == TaskKind::User => t.clone(),
            _ => return !0u64,
        }
    };

    let mut inner = task.inner.lock();

    // 3. Verify and remove the range from user_vaddr_set (NoditSet)
    let total_size = n_pages * Size4KiB::SIZE;
    if !user_vaddr::free_user_pages(&mut inner.user_vaddr_set, addr, total_size) {
        return !0u64;
    }

    // 4. Setup the Mapper using the task's cr3 and HHDM offset
    let hhdm_offset = hhdm_offset();
    let user_l4_frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(task.cr3));
    let l4_virt_addr = VirtAddr::new(hhdm_offset.as_u64() + user_l4_frame.start_address().as_u64());
    let l4_table = unsafe { &mut *l4_virt_addr.as_mut_ptr::<PageTable>() };

    let mut mapper = unsafe {
        OffsetPageTable::new(l4_table, VirtAddr::new(hhdm_offset.as_u64()))
    };

    let memory = MEMORY.get().unwrap();
    let mut physical_memory = memory.physical_memory.lock();

    // 5. Unmap each page and free its corresponding physical frame
    for i in 0..n_pages {
        let vaddr = VirtAddr::new(addr + i * Size4KiB::SIZE);
        let page: Page<Size4KiB> = Page::containing_address(vaddr);

        // x86_64 unmap returns Ok((PhysFrame, PageTableFlags, MapperFlush))
        if let Ok((frame, _, flush)) = unsafe { mapper.unmap(page) } {
            // Invalidate the TLB for this address
            flush.flush();

            // Return the frame to the physical manager (no 'Owned' wrapper needed)
            let _ = physical_memory.free_frame(frame, MemoryType::UsedByUserMode);
        }
    }

    0 // Success
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
