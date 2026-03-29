//! Syscall handlers for the handle (file-descriptor) subsystem:
//! sys_pipe, sys_read, sys_write, sys_close, sys_dup, sys_dup2.

use crate::handle::{Handle, PipeEnd, IpcChannelEnd};
use crate::memory::cpu_local_data::get_local;
use crate::pipe::{Pipe, PipeReadResult, PipeWriteResult};
use crate::task::task::{TaskKind, TaskState};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;
use super::{validate_user_ptr, current_task_and_cpu};
use kernel_api_types::*;

// ---------------------------------------------------------------------------
// sys_pipe (syscall 48)
// ---------------------------------------------------------------------------

/// Create a pipe. Writes two u32 fd numbers to user pointers.
/// Returns HANDLE_OK on success.
pub fn sys_pipe(
    read_fd_out: u64, write_fd_out: u64,
    _: u64, _: u64, _: u64, _: u64,
) -> u64 {
    let _g1 = match validate_user_ptr(read_fd_out, 4) {
        Some(g) => g,
        None => return HANDLE_ERR_INVALID_ARGS,
    };
    let _g2 = match validate_user_ptr(write_fd_out, 4) {
        Some(g) => g,
        None => return HANDLE_ERR_INVALID_ARGS,
    };

    let pipe = Pipe::new();

    let cpu = get_local();
    let rq = cpu.run_queue.get().unwrap().lock();
    let task = match &rq.current_task {
        Some(t) => t.clone(),
        None => return HANDLE_ERR_INVALID_ARGS,
    };
    drop(rq);

    let mut inner = task.inner.lock();
    let read_fd = match inner.handles.alloc(Handle::Pipe(pipe.clone(), PipeEnd::Read)) {
        Some(fd) => fd,
        None => return HANDLE_ERR_TABLE_FULL,
    };
    let write_fd = match inner.handles.alloc(Handle::Pipe(pipe, PipeEnd::Write)) {
        Some(fd) => fd,
        None => {
            // Roll back the read handle.
            inner.handles.remove(read_fd);
            return HANDLE_ERR_TABLE_FULL;
        }
    };
    drop(inner);

    unsafe {
        core::ptr::write(read_fd_out as *mut u32, read_fd);
        core::ptr::write(write_fd_out as *mut u32, write_fd);
    }

    HANDLE_OK
}

// ---------------------------------------------------------------------------
// sys_read (syscall 49)
// ---------------------------------------------------------------------------

/// Read from a handle. Returns bytes read, 0 for EOF, HANDLE_ERR_BAD_FD on error.
/// If `flags & HANDLE_NONBLOCK`, returns HANDLE_ERR_WOULD_BLOCK instead of blocking.
pub fn sys_handle_read(
    fd: u64, buf_ptr: u64, buf_len: u64,
    flags: u64, _: u64, _: u64,
) -> u64 {
    if buf_len == 0 {
        return 0;
    }

    let _g = match validate_user_ptr(buf_ptr, buf_len) {
        Some(g) => g,
        None => return HANDLE_ERR_INVALID_ARGS,
    };

    let nonblock = flags & HANDLE_NONBLOCK != 0;

    let cpu = get_local();
    let task = {
        let rq = cpu.run_queue.get().unwrap().lock();
        match &rq.current_task {
            Some(t) => t.clone(),
            None => return HANDLE_ERR_BAD_FD,
        }
    };

    // Extract what we need from the handle while holding the inner lock briefly.
    let handle_info = {
        let inner = task.inner.lock();
        match inner.handles.get(fd as u32) {
            Some(Handle::Pipe(pipe, PipeEnd::Read)) => HandleReadInfo::Pipe(pipe.clone()),
            Some(Handle::IpcChannel(ep_id, IpcChannelEnd::Recv)) => HandleReadInfo::IpcRecv(*ep_id),
            Some(Handle::Keyboard) => HandleReadInfo::Keyboard,
            Some(Handle::Null) => return 0, // EOF
            _ => return HANDLE_ERR_BAD_FD,
        }
    };

    if nonblock {
        match handle_info {
            HandleReadInfo::Pipe(pipe) => pipe_read_nonblocking(&pipe, buf_ptr, buf_len),
            HandleReadInfo::IpcRecv(ep_id) => ipc_recv_as_read_nonblocking(ep_id, buf_ptr, buf_len),
            HandleReadInfo::Keyboard => keyboard_read_nonblocking(buf_ptr, buf_len),
        }
    } else {
        match handle_info {
            HandleReadInfo::Pipe(pipe) => pipe_read_blocking(&task, &pipe, buf_ptr, buf_len),
            HandleReadInfo::IpcRecv(ep_id) => ipc_recv_as_read_blocking(&task, ep_id, buf_ptr, buf_len),
            HandleReadInfo::Keyboard => keyboard_read_blocking(&task, buf_ptr, buf_len),
        }
    }
}

enum HandleReadInfo {
    Pipe(Arc<Pipe>),
    IpcRecv(u64),
    Keyboard,
}

/// Blocking pipe read — follows the same sleep/wakeup pattern as IPC channel recv.
fn pipe_read_blocking(task: &Arc<crate::task::task::Task>, pipe: &Arc<Pipe>, buf_ptr: u64, buf_len: u64) -> u64 {
    let buf = unsafe { core::slice::from_raw_parts_mut(buf_ptr as *mut u8, buf_len as usize) };

    loop {
        match pipe.try_read(buf) {
            PipeReadResult::Ok(n) => return n as u64,
            PipeReadResult::Eof => return 0,
            PipeReadResult::WouldBlock => {
                // Set fallback return value (0 = EOF if woken spuriously and write end closes).
                let cpu = get_local();
                let ctx_ptr = cpu.current_context_ptr.load(Ordering::Relaxed);
                if !ctx_ptr.is_null() {
                    unsafe { (*ctx_ptr).rax = 0; }
                }

                // Register in read_waiters and sleep.
                {
                    let mut waiters = pipe.read_waiters.lock();
                    task.state.store(TaskState::Sleeping, Ordering::Release);
                    waiters.push_back((task.clone(), cpu.kernel_id));
                }

                // Re-check: data may have arrived or write end may have closed.
                let recheck_buf = unsafe { core::slice::from_raw_parts_mut(buf_ptr as *mut u8, buf_len as usize) };
                match pipe.try_read(recheck_buf) {
                    PipeReadResult::Ok(n) => {
                        task.state.store(TaskState::Ready, Ordering::Release);
                        // Remove ourselves from waiters (best-effort).
                        pipe.read_waiters.lock().retain(|(t, _)| t.id != task.id);
                        return n as u64;
                    }
                    PipeReadResult::Eof => {
                        task.state.store(TaskState::Ready, Ordering::Release);
                        pipe.read_waiters.lock().retain(|(t, _)| t.id != task.id);
                        return 0;
                    }
                    PipeReadResult::WouldBlock => {
                        // Actually sleep.
                        cpu.in_syscall_handler.store(0, Ordering::Relaxed);
                        x86_64::instructions::interrupts::enable();
                        x86_64::instructions::hlt();
                        x86_64::instructions::interrupts::disable();
                        // Loop back and retry.
                    }
                }
            }
        }
    }
}

/// Blocking IPC recv as byte-stream read. Mirrors sys_channel_recv blocking pattern.
fn ipc_recv_as_read_blocking(task: &Arc<crate::task::task::Task>, ep_id: u64, buf_ptr: u64, buf_len: u64) -> u64 {
    // Get the channel Arc for waiter registration.
    let channel_arc = {
        let registry = crate::ipc::ENDPOINT_REGISTRY.lock();
        match registry.get(&ep_id) {
            Some(ep) if ep.role == crate::ipc::EndpointRole::Recv => ep.channel.clone(),
            _ => return HANDLE_ERR_BAD_FD,
        }
    };

    loop {
        match crate::ipc::try_recv(ep_id) {
            Ok(msg) => {
                let copy_len = msg.len().min(buf_len as usize);
                unsafe {
                    core::ptr::copy_nonoverlapping(msg.as_ptr(), buf_ptr as *mut u8, copy_len);
                }
                return copy_len as u64;
            }
            Err(crate::ipc::IpcError::PeerClosed) => return 0, // EOF
            Err(crate::ipc::IpcError::WouldBlock) => {
                // Set fallback return value (0 = EOF on spurious wake).
                let cpu = get_local();
                let ctx_ptr = cpu.current_context_ptr.load(Ordering::Relaxed);
                if !ctx_ptr.is_null() {
                    unsafe { (*ctx_ptr).rax = 0; }
                }

                // Register as recv waiter and sleep.
                {
                    let mut waiters = channel_arc.recv_waiters.lock();
                    waiters.retain(|(t, _)| !alloc::sync::Arc::ptr_eq(t, task));
                    task.state.store(TaskState::Sleeping, Ordering::Release);
                    waiters.push_back((task.clone(), cpu.kernel_id));
                }

                // Re-check: message may have arrived between try_recv and registration.
                if crate::ipc::channel_has_message(ep_id)
                    || channel_arc.send_closed.load(Ordering::Acquire)
                {
                    task.state.store(TaskState::Ready, Ordering::Release);
                    channel_arc.recv_waiters.lock()
                        .retain(|(t, _)| !alloc::sync::Arc::ptr_eq(t, task));
                    continue;
                }

                cpu.in_syscall_handler.store(0, Ordering::Relaxed);
                x86_64::instructions::interrupts::enable();
                x86_64::instructions::hlt();
                x86_64::instructions::interrupts::disable();
                // Loop and retry.
            }
            Err(_) => return HANDLE_ERR_BAD_FD,
        }
    }
}

/// Blocking keyboard read — returns KeyEvent structs as raw bytes.
fn keyboard_read_blocking(task: &Arc<crate::task::task::Task>, buf_ptr: u64, buf_len: u64) -> u64 {
    let key_size = core::mem::size_of::<kernel_api_types::KeyEvent>();
    if (buf_len as usize) < key_size {
        return HANDLE_ERR_INVALID_ARGS;
    }

    loop {
        if let Some(event) = crate::drivers::keyboard::try_read_key() {
            // Copy KeyEvent bytes to user buffer.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    &event as *const kernel_api_types::KeyEvent as *const u8,
                    buf_ptr as *mut u8,
                    key_size,
                );
            }
            return key_size as u64;
        }

        // Set fallback return value (0 = no data on spurious wake).
        let cpu = get_local();
        let ctx_ptr = cpu.current_context_ptr.load(Ordering::Relaxed);
        if !ctx_ptr.is_null() {
            unsafe { (*ctx_ptr).rax = 0; }
        }

        // Register as keyboard waiter and sleep.
        {
            let mut slot = crate::drivers::keyboard::KEYBOARD_WAITER.lock();
            task.state.store(TaskState::Sleeping, Ordering::Release);
            *slot = Some((task.clone(), cpu.kernel_id));
        }

        // Re-check: key may have arrived between try_read_key and registration.
        if crate::drivers::keyboard::has_key() {
            task.state.store(TaskState::Ready, Ordering::Release);
            *crate::drivers::keyboard::KEYBOARD_WAITER.lock() = None;
            continue;
        }

        cpu.in_syscall_handler.store(0, Ordering::Relaxed);
        x86_64::instructions::interrupts::enable();
        x86_64::instructions::hlt();
        x86_64::instructions::interrupts::disable();
    }
}

// ---------------------------------------------------------------------------
// Non-blocking read helpers
// ---------------------------------------------------------------------------

fn pipe_read_nonblocking(pipe: &Arc<Pipe>, buf_ptr: u64, buf_len: u64) -> u64 {
    let buf = unsafe { core::slice::from_raw_parts_mut(buf_ptr as *mut u8, buf_len as usize) };
    match pipe.try_read(buf) {
        PipeReadResult::Ok(n) => n as u64,
        PipeReadResult::Eof => 0,
        PipeReadResult::WouldBlock => HANDLE_ERR_WOULD_BLOCK,
    }
}

fn ipc_recv_as_read_nonblocking(ep_id: u64, buf_ptr: u64, buf_len: u64) -> u64 {
    match crate::ipc::try_recv(ep_id) {
        Ok(msg) => {
            let copy_len = msg.len().min(buf_len as usize);
            unsafe {
                core::ptr::copy_nonoverlapping(msg.as_ptr(), buf_ptr as *mut u8, copy_len);
            }
            copy_len as u64
        }
        Err(crate::ipc::IpcError::PeerClosed) => 0,
        Err(crate::ipc::IpcError::WouldBlock) => HANDLE_ERR_WOULD_BLOCK,
        Err(_) => HANDLE_ERR_BAD_FD,
    }
}

fn keyboard_read_nonblocking(buf_ptr: u64, buf_len: u64) -> u64 {
    let key_size = core::mem::size_of::<kernel_api_types::KeyEvent>();
    if (buf_len as usize) < key_size {
        return HANDLE_ERR_INVALID_ARGS;
    }
    match crate::drivers::keyboard::try_read_key() {
        Some(event) => {
            unsafe {
                core::ptr::copy_nonoverlapping(
                    &event as *const kernel_api_types::KeyEvent as *const u8,
                    buf_ptr as *mut u8,
                    key_size,
                );
            }
            key_size as u64
        }
        None => HANDLE_ERR_WOULD_BLOCK,
    }
}

// ---------------------------------------------------------------------------
// sys_write (syscall 50)
// ---------------------------------------------------------------------------

/// Write to a handle. Returns bytes written, HANDLE_ERR_BROKEN_PIPE, or HANDLE_ERR_BAD_FD.
/// If `flags & HANDLE_NONBLOCK`, returns HANDLE_ERR_WOULD_BLOCK instead of blocking.
pub fn sys_handle_write(
    fd: u64, buf_ptr: u64, buf_len: u64,
    flags: u64, _: u64, _: u64,
) -> u64 {
    if buf_len == 0 {
        return 0;
    }

    let _g = match validate_user_ptr(buf_ptr, buf_len) {
        Some(g) => g,
        None => return HANDLE_ERR_INVALID_ARGS,
    };

    let nonblock = flags & HANDLE_NONBLOCK != 0;

    let cpu = get_local();
    let task = {
        let rq = cpu.run_queue.get().unwrap().lock();
        match &rq.current_task {
            Some(t) => t.clone(),
            None => return HANDLE_ERR_BAD_FD,
        }
    };

    let handle_info = {
        let inner = task.inner.lock();
        match inner.handles.get(fd as u32) {
            Some(Handle::Pipe(pipe, PipeEnd::Write)) => HandleWriteInfo::Pipe(pipe.clone()),
            Some(Handle::IpcChannel(ep_id, IpcChannelEnd::Send)) => HandleWriteInfo::IpcSend(*ep_id),
            Some(Handle::Null) => return buf_len, // discard silently
            _ => return HANDLE_ERR_BAD_FD,
        }
    };

    if nonblock {
        match handle_info {
            HandleWriteInfo::Pipe(pipe) => pipe_write_nonblocking(&pipe, buf_ptr, buf_len),
            HandleWriteInfo::IpcSend(ep_id) => ipc_send_as_write(ep_id, buf_ptr, buf_len),
        }
    } else {
        match handle_info {
            HandleWriteInfo::Pipe(pipe) => pipe_write_blocking(&task, &pipe, buf_ptr, buf_len),
            HandleWriteInfo::IpcSend(ep_id) => ipc_send_as_write(ep_id, buf_ptr, buf_len),
        }
    }
}

enum HandleWriteInfo {
    Pipe(Arc<Pipe>),
    IpcSend(u64),
}

/// Blocking pipe write.
fn pipe_write_blocking(task: &Arc<crate::task::task::Task>, pipe: &Arc<Pipe>, buf_ptr: u64, buf_len: u64) -> u64 {
    let data = unsafe { core::slice::from_raw_parts(buf_ptr as *const u8, buf_len as usize) };
    let mut total_written: usize = 0;

    while total_written < data.len() {
        match pipe.try_write(&data[total_written..]) {
            PipeWriteResult::Ok(n) => {
                total_written += n;
            }
            PipeWriteResult::BrokenPipe => {
                if total_written > 0 {
                    return total_written as u64;
                }
                return HANDLE_ERR_BROKEN_PIPE;
            }
            PipeWriteResult::WouldBlock => {
                if total_written > 0 {
                    // Return partial write (like Unix short write).
                    return total_written as u64;
                }

                // Block until space is available.
                let cpu = get_local();
                let ctx_ptr = cpu.current_context_ptr.load(Ordering::Relaxed);
                if !ctx_ptr.is_null() {
                    unsafe { (*ctx_ptr).rax = HANDLE_ERR_BROKEN_PIPE; }
                }

                {
                    let mut waiters = pipe.write_waiters.lock();
                    task.state.store(TaskState::Sleeping, Ordering::Release);
                    waiters.push_back((task.clone(), cpu.kernel_id));
                }

                // Re-check.
                match pipe.try_write(data) {
                    PipeWriteResult::Ok(n) => {
                        task.state.store(TaskState::Ready, Ordering::Release);
                        pipe.write_waiters.lock().retain(|(t, _)| t.id != task.id);
                        total_written += n;
                        continue;
                    }
                    PipeWriteResult::BrokenPipe => {
                        task.state.store(TaskState::Ready, Ordering::Release);
                        pipe.write_waiters.lock().retain(|(t, _)| t.id != task.id);
                        return HANDLE_ERR_BROKEN_PIPE;
                    }
                    PipeWriteResult::WouldBlock => {
                        cpu.in_syscall_handler.store(0, Ordering::Relaxed);
                        x86_64::instructions::interrupts::enable();
                        x86_64::instructions::hlt();
                        x86_64::instructions::interrupts::disable();
                        // Loop back and retry.
                    }
                }
            }
        }
    }

    total_written as u64
}

/// Send buffer as a single IPC message (non-blocking by nature).
fn ipc_send_as_write(ep_id: u64, buf_ptr: u64, buf_len: u64) -> u64 {
    let data = unsafe { core::slice::from_raw_parts(buf_ptr as *const u8, buf_len as usize) };
    match crate::ipc::try_send(ep_id, data) {
        Ok(()) => buf_len,
        Err(crate::ipc::IpcError::PeerClosed) => HANDLE_ERR_BROKEN_PIPE,
        Err(crate::ipc::IpcError::WouldBlock) | Err(crate::ipc::IpcError::ChannelFull) => HANDLE_ERR_WOULD_BLOCK,
        Err(_) => HANDLE_ERR_BAD_FD,
    }
}

fn pipe_write_nonblocking(pipe: &Arc<Pipe>, buf_ptr: u64, buf_len: u64) -> u64 {
    let data = unsafe { core::slice::from_raw_parts(buf_ptr as *const u8, buf_len as usize) };
    match pipe.try_write(data) {
        PipeWriteResult::Ok(n) => n as u64,
        PipeWriteResult::BrokenPipe => HANDLE_ERR_BROKEN_PIPE,
        PipeWriteResult::WouldBlock => HANDLE_ERR_WOULD_BLOCK,
    }
}

// ---------------------------------------------------------------------------
// sys_close (syscall 51)
// ---------------------------------------------------------------------------

/// Close a handle. Returns 0 on success, HANDLE_ERR_BAD_FD if fd is invalid.
pub fn sys_handle_close(
    fd: u64, _: u64, _: u64, _: u64, _: u64, _: u64,
) -> u64 {
    let cpu = get_local();
    let task = {
        let rq = cpu.run_queue.get().unwrap().lock();
        match &rq.current_task {
            Some(t) => t.clone(),
            None => return HANDLE_ERR_BAD_FD,
        }
    };

    let handle = {
        let mut inner = task.inner.lock();
        match inner.handles.remove(fd as u32) {
            Some(h) => h,
            None => return HANDLE_ERR_BAD_FD,
        }
    };

    close_handle(handle);
    HANDLE_OK
}

/// Close a handle, updating pipe closed flags.
pub(crate) fn close_handle(handle: Handle) {
    match handle {
        Handle::Pipe(pipe, PipeEnd::Read) => {
            pipe.read_closed.store(true, Ordering::Release);
            // Wake any writers blocked on a full pipe so they see BrokenPipe.
            while let Some((task, cpu_id)) = pipe.write_waiters.lock().pop_front() {
                task.state.store(TaskState::Ready, Ordering::Release);
                crate::task::local_scheduler::add_front(
                    crate::memory::cpu_local_data::get_cpu(cpu_id),
                    task,
                );
            }
        }
        Handle::Pipe(pipe, PipeEnd::Write) => {
            pipe.write_closed.store(true, Ordering::Release);
            // Wake any readers blocked on an empty pipe so they see EOF.
            while let Some((task, cpu_id)) = pipe.read_waiters.lock().pop_front() {
                task.state.store(TaskState::Ready, Ordering::Release);
                crate::task::local_scheduler::add_front(
                    crate::memory::cpu_local_data::get_cpu(cpu_id),
                    task,
                );
            }
            crate::task::local_scheduler::try_wake_slot(&pipe.event_waiter);
        }
        Handle::IpcChannel(ep_id, _) => {
            // Each handle owns its own cloned endpoint (clone-on-wrap).
            // Releasing it decrements handle_refs and closes at 0.
            let _ = crate::ipc::release_endpoint(ep_id);
        }
        Handle::Keyboard => {
            // Nothing to release — keyboard is a shared global resource.
        }
        Handle::Null => {}
    }
}

// ---------------------------------------------------------------------------
// sys_dup (syscall 52)
// ---------------------------------------------------------------------------

/// Duplicate a handle to the lowest free slot. Returns new fd, or HANDLE_ERR_BAD_FD.
pub fn sys_handle_dup(
    old_fd: u64, _: u64, _: u64, _: u64, _: u64, _: u64,
) -> u64 {
    let cpu = get_local();
    let task = {
        let rq = cpu.run_queue.get().unwrap().lock();
        match &rq.current_task {
            Some(t) => t.clone(),
            None => return HANDLE_ERR_BAD_FD,
        }
    };

    let mut inner = task.inner.lock();
    let cloned = match inner.handles.get(old_fd as u32) {
        Some(h) => match h.try_clone() {
            Some(c) => c,
            None => return HANDLE_ERR_BAD_FD,
        },
        None => return HANDLE_ERR_BAD_FD,
    };

    // Track cloned IPC endpoint for cleanup on task exit.
    if let Handle::IpcChannel(ep_id, _) = &cloned {
        inner.owned_endpoints.push(*ep_id);
    }

    match inner.handles.alloc(cloned) {
        Some(fd) => fd as u64,
        None => HANDLE_ERR_TABLE_FULL,
    }
}

// ---------------------------------------------------------------------------
// sys_dup2 (syscall 53)
// ---------------------------------------------------------------------------

/// Duplicate a handle to a specific slot. Returns new_fd on success.
pub fn sys_handle_dup2(
    old_fd: u64, new_fd: u64,
    _: u64, _: u64, _: u64, _: u64,
) -> u64 {
    if new_fd >= crate::handle::MAX_HANDLES as u64 {
        return HANDLE_ERR_BAD_FD;
    }
    if old_fd == new_fd {
        return new_fd; // no-op, like POSIX dup2
    }

    let cpu = get_local();
    let task = {
        let rq = cpu.run_queue.get().unwrap().lock();
        match &rq.current_task {
            Some(t) => t.clone(),
            None => return HANDLE_ERR_BAD_FD,
        }
    };

    let mut inner = task.inner.lock();
    let cloned = match inner.handles.get(old_fd as u32) {
        Some(h) => match h.try_clone() {
            Some(c) => c,
            None => return HANDLE_ERR_BAD_FD,
        },
        None => return HANDLE_ERR_BAD_FD,
    };

    // Track cloned IPC endpoint for cleanup on task exit.
    if let Handle::IpcChannel(ep_id, _) = &cloned {
        inner.owned_endpoints.push(*ep_id);
    }

    // Close whatever was at new_fd.
    if let Some(old_handle) = inner.handles.alloc_at(new_fd as u32, cloned) {
        drop(inner); // Release lock before close_handle which may wake tasks.
        close_handle(old_handle);
    }

    new_fd
}

// ---------------------------------------------------------------------------
// sys_handle_from_channel (syscall 55)
// ---------------------------------------------------------------------------

/// Clone an IPC endpoint and wrap it as a handle.
///
/// Creates a **new** endpoint on the same channel (like `dup()` on a Unix fd).
/// Closing the handle only destroys this cloned endpoint, not the original.
///
/// `direction`: 0 = recv (readable), 1 = send (writable).
/// Returns the new fd, or HANDLE_ERR_BAD_FD / HANDLE_ERR_TABLE_FULL.
pub fn sys_handle_from_channel(
    endpoint_id: u64, direction: u64,
    _: u64, _: u64, _: u64, _: u64,
) -> u64 {
    // Validate direction matches the endpoint's role before cloning.
    {
        let registry = crate::ipc::ENDPOINT_REGISTRY.lock();
        match registry.get(&endpoint_id) {
            Some(ep) => {
                let want_recv = direction == 0;
                let is_recv = ep.role == crate::ipc::EndpointRole::Recv;
                if want_recv != is_recv {
                    return HANDLE_ERR_INVALID_ARGS;
                }
            }
            None => return HANDLE_ERR_BAD_FD,
        }
    }

    // Clone the endpoint — creates a new endpoint ID on the same channel.
    let new_ep_id = match crate::ipc::clone_endpoint(endpoint_id) {
        Ok(id) => id,
        Err(_) => return HANDLE_ERR_BAD_FD,
    };

    let dir = if direction == 0 {
        IpcChannelEnd::Recv
    } else {
        IpcChannelEnd::Send
    };

    let cpu = get_local();
    let task = {
        let rq = cpu.run_queue.get().unwrap().lock();
        match &rq.current_task {
            Some(t) => t.clone(),
            None => return HANDLE_ERR_BAD_FD,
        }
    };

    let mut inner = task.inner.lock();
    // Track the cloned endpoint for cleanup on task exit.
    inner.owned_endpoints.push(new_ep_id);
    match inner.handles.alloc(Handle::IpcChannel(new_ep_id, dir)) {
        Some(fd) => fd as u64,
        None => {
            // Rollback: destroy the cloned endpoint.
            inner.owned_endpoints.retain(|&id| id != new_ep_id);
            let _ = crate::ipc::close_endpoint(new_ep_id);
            HANDLE_ERR_TABLE_FULL
        }
    }
}

// ---------------------------------------------------------------------------
// sys_handle_channel_create (syscall 57)
// ---------------------------------------------------------------------------

/// Create a new IPC channel and return both endpoints as handles.
///
/// Writes `send_fd` and `recv_fd` (u32) to the user-provided output pointers.
/// Returns HANDLE_OK on success.
pub fn sys_handle_channel_create(
    send_fd_out: u64, recv_fd_out: u64, capacity: u64,
    _: u64, _: u64, _: u64,
) -> u64 {
    let _g1 = match validate_user_ptr(send_fd_out, 4) {
        Some(g) => g,
        None => return HANDLE_ERR_INVALID_ARGS,
    };
    let _g2 = match validate_user_ptr(recv_fd_out, 4) {
        Some(g) => g,
        None => return HANDLE_ERR_INVALID_ARGS,
    };

    let cap = if capacity == 0 {
        crate::ipc::DEFAULT_CHANNEL_CAPACITY
    } else {
        (capacity as usize).clamp(1, crate::ipc::MAX_CHANNEL_CAPACITY)
    };

    let (send_ep, recv_ep) = crate::ipc::create_channel(cap);

    // Set handle_refs = 1 on both endpoints (they'll each be wrapped by a handle).
    {
        let mut registry = crate::ipc::ENDPOINT_REGISTRY.lock();
        if let Some(ep) = registry.get_mut(&send_ep) { ep.handle_refs = 1; }
        if let Some(ep) = registry.get_mut(&recv_ep) { ep.handle_refs = 1; }
    }

    let cpu = get_local();
    let task = {
        let rq = cpu.run_queue.get().unwrap().lock();
        match &rq.current_task {
            Some(t) => t.clone(),
            None => return HANDLE_ERR_INVALID_ARGS,
        }
    };

    let mut inner = task.inner.lock();

    let send_fd = match inner.handles.alloc(Handle::IpcChannel(send_ep, IpcChannelEnd::Send)) {
        Some(fd) => fd,
        None => {
            let _ = crate::ipc::close_endpoint(send_ep);
            let _ = crate::ipc::close_endpoint(recv_ep);
            return HANDLE_ERR_TABLE_FULL;
        }
    };
    let recv_fd = match inner.handles.alloc(Handle::IpcChannel(recv_ep, IpcChannelEnd::Recv)) {
        Some(fd) => fd,
        None => {
            inner.handles.remove(send_fd);
            let _ = crate::ipc::close_endpoint(send_ep);
            let _ = crate::ipc::close_endpoint(recv_ep);
            return HANDLE_ERR_TABLE_FULL;
        }
    };

    // Track for cleanup on exit.
    inner.owned_endpoints.push(send_ep);
    inner.owned_endpoints.push(recv_ep);
    drop(inner);

    unsafe {
        core::ptr::write(send_fd_out as *mut u32, send_fd);
        core::ptr::write(recv_fd_out as *mut u32, recv_fd);
    }

    HANDLE_OK
}

// ---------------------------------------------------------------------------
// sys_handle_open_keyboard (syscall 56)
// ---------------------------------------------------------------------------

/// Create a keyboard handle. Returns the new fd.
/// Reading from this handle blocks until a KeyEvent is available,
/// then returns the KeyEvent struct as raw bytes.
pub fn sys_handle_open_keyboard(
    _: u64, _: u64, _: u64, _: u64, _: u64, _: u64,
) -> u64 {
    let cpu = get_local();
    let task = {
        let rq = cpu.run_queue.get().unwrap().lock();
        match &rq.current_task {
            Some(t) => t.clone(),
            None => return HANDLE_ERR_BAD_FD,
        }
    };

    let mut inner = task.inner.lock();
    match inner.handles.alloc(Handle::Keyboard) {
        Some(fd) => fd as u64,
        None => HANDLE_ERR_TABLE_FULL,
    }
}

// ---------------------------------------------------------------------------
// sys_handle_wait (syscall 58)
// ---------------------------------------------------------------------------

/// Like `sys_wait_for_event` but takes handle fds (u32 array) instead of raw
/// endpoint IDs. Resolves each fd to its underlying IPC endpoint, then
/// delegates to the existing wait logic.
///
/// Arguments:
///   fds_ptr       – userspace pointer to array of u32 handle fds
///   fd_count      – number of entries (clamped to 64)
///   flags         – WAIT_KEYBOARD (1) | WAIT_MOUSE (2)
///   timeout_ms    – 0 = infinite; non-zero = wake after this many ms
///
/// Returns: 0 = event available, 1 = timed out, 2 = invalid args
pub fn sys_handle_wait(
    fds_ptr: u64,
    fd_count: u64,
    flags: u64,
    timeout_ms: u64,
    _: u64,
    _: u64,
) -> u64 {
    const MAX_FDS: usize = 64;
    const RESULT_INVALID: u64 = 2;

    let count = fd_count.min(MAX_FDS as u64) as usize;

    // Read handle fds from userspace and resolve to endpoint IDs.
    let mut ep_ids = [0u64; MAX_FDS];
    if count > 0 {
        let _guard = match validate_user_ptr(fds_ptr, (count as u64) * 4) {
            Some(g) => g,
            None => return RESULT_INVALID,
        };

        let cpu = get_local();
        let task = {
            let rq = cpu.run_queue.get().unwrap().lock();
            match &rq.current_task {
                Some(t) => t.clone(),
                None => return RESULT_INVALID,
            }
        };
        let inner = task.inner.lock();

        for i in 0..count {
            let fd = unsafe { core::ptr::read_unaligned((fds_ptr as *const u32).add(i)) };
            match inner.handles.get(fd) {
                Some(Handle::IpcChannel(ep_id, IpcChannelEnd::Recv)) => {
                    ep_ids[i] = *ep_id;
                }
                _ => return RESULT_INVALID,
            }
        }
        drop(inner);
    }

    // Write the resolved endpoint IDs to a userspace-like buffer and delegate
    // to the existing sys_wait_for_event implementation.
    // We can call it directly since we have the resolved ep_ids on the kernel stack.
    // But sys_wait_for_event reads from a user pointer — instead, we inline the
    // core wait logic by calling the event module directly.
    crate::syscall_handlers::event::wait_for_event_inner(&ep_ids[..count], flags, timeout_ms)
}

// ---------------------------------------------------------------------------
// sys_handle_register_service (syscall 59)
// ---------------------------------------------------------------------------

/// Register a service using a handle fd instead of a raw endpoint ID.
/// The handle must be a Send IPC channel handle.
///
/// Arguments: name_ptr, name_len, send_fd
/// Returns: SVC_OK or SVC_ERR_*
pub fn sys_handle_register_service(
    name_ptr: u64, name_len: u64, send_fd: u64,
    _: u64, _: u64, _: u64,
) -> u64 {
    use kernel_api_types::*;

    if name_len == 0 || name_len > MAX_SERVICE_NAME_LEN as u64 {
        return SVC_ERR_INVALID_ARGS;
    }
    let _guard = match validate_user_ptr(name_ptr, name_len) {
        Some(g) => g,
        None => return SVC_ERR_INVALID_ARGS,
    };

    let cpu = get_local();
    let task = {
        let rq = cpu.run_queue.get().unwrap().lock();
        match &rq.current_task {
            Some(t) => t.clone(),
            None => return SVC_ERR_INVALID_ARGS,
        }
    };

    // Resolve handle fd to endpoint ID.
    let ep_id = {
        let inner = task.inner.lock();
        match inner.handles.get(send_fd as u32) {
            Some(Handle::IpcChannel(ep_id, IpcChannelEnd::Send)) => *ep_id,
            _ => return SVC_ERR_INVALID_ARGS,
        }
    };

    // Verify the endpoint exists and is a Send endpoint.
    {
        let registry = crate::ipc::ENDPOINT_REGISTRY.lock();
        match registry.get(&ep_id) {
            Some(ep) if ep.role == crate::ipc::EndpointRole::Send => {}
            _ => return SVC_ERR_INVALID_ARGS,
        }
    }

    let name_bytes = unsafe {
        core::slice::from_raw_parts(name_ptr as *const u8, name_len as usize)
    };

    match crate::service_registry::register(name_bytes, ep_id, task.id) {
        Ok(()) => {
            let name_str = core::str::from_utf8(name_bytes).unwrap_or("?");
            log::info!("sys_handle_register_service: ok name={:?} ep={}", name_str, ep_id);
            let mut name_arr = [0u8; MAX_SERVICE_NAME_LEN];
            let copy_len = name_bytes.len().min(MAX_SERVICE_NAME_LEN);
            name_arr[..copy_len].copy_from_slice(&name_bytes[..copy_len]);
            task.inner.lock().registered_services.push(name_arr);
            SVC_OK
        }
        Err(code) => code,
    }
}
