//! Handle (file descriptor) API for Bos OS.
//!
//! Provides Unix-like `pipe()`, `read()`, `write()`, `close()`, `dup()`, `dup2()`
//! operating on per-task handles. Handle 0/1/2 are stdin/stdout/stderr by convention.

use kernel_api_types::SysCallNumber;

/// Create a pipe. Returns `(read_fd, write_fd)`, or `None` on error.
pub fn pipe() -> Option<(u32, u32)> {
    let mut read_fd: u32 = 0;
    let mut write_fd: u32 = 0;
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::Pipe as u64;
    args[1] = &mut read_fd as *mut u32 as u64;
    args[2] = &mut write_fd as *mut u32 as u64;
    crate::syscall(&mut args);
    if args[6] == kernel_api_types::HANDLE_OK {
        Some((read_fd, write_fd))
    } else {
        None
    }
}

/// Read from a handle (blocking). Returns bytes read (0 = EOF).
/// Returns `None` on bad fd or invalid args.
pub fn read(fd: u32, buf: &mut [u8]) -> Option<usize> {
    read_flags(fd, buf, 0)
}

/// Non-blocking read. Returns bytes read, or `None` on error/would-block.
/// Use `try_read_raw` if you need to distinguish would-block from errors.
pub fn try_read(fd: u32, buf: &mut [u8]) -> Option<usize> {
    read_flags(fd, buf, kernel_api_types::HANDLE_NONBLOCK)
}

/// Raw read with flags. Returns the raw syscall result.
fn read_flags(fd: u32, buf: &mut [u8], flags: u64) -> Option<usize> {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::HandleRead as u64;
    args[1] = fd as u64;
    args[2] = buf.as_mut_ptr() as u64;
    args[3] = buf.len() as u64;
    args[4] = flags;
    crate::syscall(&mut args);
    let ret = args[6];
    if ret == kernel_api_types::HANDLE_ERR_BAD_FD
        || ret == kernel_api_types::HANDLE_ERR_INVALID_ARGS
        || ret == kernel_api_types::HANDLE_ERR_WOULD_BLOCK
    {
        None
    } else {
        Some(ret as usize)
    }
}

/// Write to a handle (blocking). Returns bytes written.
/// Returns `None` on bad fd, broken pipe, or invalid args.
pub fn write(fd: u32, buf: &[u8]) -> Option<usize> {
    write_flags(fd, buf, 0)
}

/// Non-blocking write. Returns bytes written, or `None` on error/would-block.
pub fn try_write(fd: u32, buf: &[u8]) -> Option<usize> {
    write_flags(fd, buf, kernel_api_types::HANDLE_NONBLOCK)
}

/// Raw non-blocking write that returns the raw syscall u64 result.
/// Callers that need to distinguish WOULD_BLOCK from BROKEN_PIPE use this.
pub fn try_write_raw(fd: u32, buf: &[u8]) -> u64 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::HandleWrite as u64;
    args[1] = fd as u64;
    args[2] = buf.as_ptr() as u64;
    args[3] = buf.len() as u64;
    args[4] = kernel_api_types::HANDLE_NONBLOCK;
    crate::syscall(&mut args);
    args[6]
}

fn write_flags(fd: u32, buf: &[u8], flags: u64) -> Option<usize> {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::HandleWrite as u64;
    args[1] = fd as u64;
    args[2] = buf.as_ptr() as u64;
    args[3] = buf.len() as u64;
    args[4] = flags;
    crate::syscall(&mut args);
    let ret = args[6];
    if ret == kernel_api_types::HANDLE_ERR_BAD_FD
        || ret == kernel_api_types::HANDLE_ERR_BROKEN_PIPE
        || ret == kernel_api_types::HANDLE_ERR_INVALID_ARGS
        || ret == kernel_api_types::HANDLE_ERR_WOULD_BLOCK
    {
        None
    } else {
        Some(ret as usize)
    }
}

/// Close a handle. Returns true on success.
pub fn close(fd: u32) -> bool {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::HandleClose as u64;
    args[1] = fd as u64;
    crate::syscall(&mut args);
    args[6] == kernel_api_types::HANDLE_OK
}

/// Duplicate a handle to the lowest free fd. Returns the new fd, or `None` on error.
pub fn dup(fd: u32) -> Option<u32> {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::HandleDup as u64;
    args[1] = fd as u64;
    crate::syscall(&mut args);
    let ret = args[6];
    if ret == kernel_api_types::HANDLE_ERR_BAD_FD
        || ret == kernel_api_types::HANDLE_ERR_TABLE_FULL
    {
        None
    } else {
        Some(ret as u32)
    }
}

/// Duplicate a handle to a specific fd (closing whatever was there).
/// Returns the new fd, or `None` on error.
pub fn dup2(old_fd: u32, new_fd: u32) -> Option<u32> {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::HandleDup2 as u64;
    args[1] = old_fd as u64;
    args[2] = new_fd as u64;
    crate::syscall(&mut args);
    let ret = args[6];
    if ret == kernel_api_types::HANDLE_ERR_BAD_FD {
        None
    } else {
        Some(ret as u32)
    }
}

/// Spawn a new task with inherited handles.
///
/// `mappings` is a slice of `(parent_fd, child_fd)` pairs. Each parent handle
/// is cloned and installed at the given fd in the child's handle table.
///
/// Returns the child task ID, or 0 on failure.
pub fn spawn_with_handles(
    elf: &[u8],
    child_arg: u64,
    name: &[u8],
    priority: u8,
    mappings: &[(u32, u32)],
) -> u64 {
    let desc = kernel_api_types::SpawnHandlesDesc {
        priority,
        _pad: [0; 3],
        handle_count: mappings.len() as u32,
    };

    let desc_size = core::mem::size_of::<kernel_api_types::SpawnHandlesDesc>();
    let mapping_size = core::mem::size_of::<kernel_api_types::HandleMapping>();
    let total_size = desc_size + mappings.len() * mapping_size;

    let mut buf = [0u8; 512]; // enough for 62 mappings
    if total_size > buf.len() {
        return 0;
    }

    unsafe {
        core::ptr::copy_nonoverlapping(
            &desc as *const _ as *const u8,
            buf.as_mut_ptr(),
            desc_size,
        );
        for (i, &(parent_fd, child_fd)) in mappings.iter().enumerate() {
            let m = kernel_api_types::HandleMapping { parent_fd, child_fd };
            core::ptr::copy_nonoverlapping(
                &m as *const _ as *const u8,
                buf.as_mut_ptr().add(desc_size + i * mapping_size),
                mapping_size,
            );
        }
    }

    let mut args = [0u64; 7];
    args[0] = SysCallNumber::SpawnWithHandles as u64;
    args[1] = elf.as_ptr() as u64;
    args[2] = elf.len() as u64;
    args[3] = child_arg;
    args[4] = name.as_ptr() as u64;
    args[5] = name.len() as u64;
    args[6] = buf.as_ptr() as u64;
    crate::syscall(&mut args);
    args[6]
}

/// Wrap an existing IPC endpoint as a handle (file descriptor).
///
/// `direction`: 0 = recv (readable), 1 = send (writable).
/// Returns the new fd, or `None` on error.
pub fn handle_from_channel(endpoint_id: u64, direction: u32) -> Option<u32> {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::HandleFromChannel as u64;
    args[1] = endpoint_id;
    args[2] = direction as u64;
    crate::syscall(&mut args);
    let ret = args[6];
    if ret == kernel_api_types::HANDLE_ERR_BAD_FD
        || ret == kernel_api_types::HANDLE_ERR_TABLE_FULL
        || ret == kernel_api_types::HANDLE_ERR_INVALID_ARGS
    {
        None
    } else {
        Some(ret as u32)
    }
}

/// Create a keyboard handle. Reading from it returns raw `KeyEvent` structs as bytes.
///
/// Returns the new fd, or `None` if the handle table is full.
pub fn open_keyboard() -> Option<u32> {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::HandleOpenKeyboard as u64;
    crate::syscall(&mut args);
    let ret = args[6];
    if ret == kernel_api_types::HANDLE_ERR_BAD_FD
        || ret == kernel_api_types::HANDLE_ERR_TABLE_FULL
    {
        None
    } else {
        Some(ret as u32)
    }
}

/// Read a `KeyEvent` from a keyboard handle. Blocks until a key is available.
pub fn read_key_event(fd: u32) -> Option<kernel_api_types::KeyEvent> {
    let mut event = kernel_api_types::KeyEvent::EMPTY;
    let buf = unsafe {
        core::slice::from_raw_parts_mut(
            &mut event as *mut kernel_api_types::KeyEvent as *mut u8,
            core::mem::size_of::<kernel_api_types::KeyEvent>(),
        )
    };
    match read(fd, buf) {
        Some(n) if n == core::mem::size_of::<kernel_api_types::KeyEvent>() => Some(event),
        _ => None,
    }
}

/// Try to read a `KeyEvent` from a keyboard handle (non-blocking).
pub fn try_read_key_event(fd: u32) -> Option<kernel_api_types::KeyEvent> {
    let mut event = kernel_api_types::KeyEvent::EMPTY;
    let buf = unsafe {
        core::slice::from_raw_parts_mut(
            &mut event as *mut kernel_api_types::KeyEvent as *mut u8,
            core::mem::size_of::<kernel_api_types::KeyEvent>(),
        )
    };
    match try_read(fd, buf) {
        Some(n) if n == core::mem::size_of::<kernel_api_types::KeyEvent>() => Some(event),
        _ => None,
    }
}

/// Create an IPC channel and wrap both endpoints as handles.
///
/// Returns `(send_fd, recv_fd)`, or `None` on failure.
/// This is the handle-based replacement for `sys_channel_create`.
pub fn channel(capacity: u64) -> Option<(u32, u32)> {
    let mut send_fd: u32 = 0;
    let mut recv_fd: u32 = 0;
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::HandleChannelCreate as u64;
    args[1] = &mut send_fd as *mut u32 as u64;
    args[2] = &mut recv_fd as *mut u32 as u64;
    args[3] = capacity;
    crate::syscall(&mut args);
    if args[6] == kernel_api_types::HANDLE_OK {
        Some((send_fd, recv_fd))
    } else {
        None
    }
}

/// Block until any watched event source has data (or timeout expires).
///
/// `fds` — slice of recv handle fds to watch for incoming messages.
/// `flags` — `WAIT_KEYBOARD` (1) | `WAIT_MOUSE` (2).
/// `timeout_ms` — 0 = infinite; non-zero = wake after this many ms.
///
/// Returns: 0 = event available, 1 = timed out, 2 = invalid args.
pub fn wait(fds: &[u32], flags: u32, timeout_ms: u64) -> u64 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::HandleWait as u64;
    args[1] = fds.as_ptr() as u64;
    args[2] = fds.len() as u64;
    args[3] = flags as u64;
    args[4] = timeout_ms;
    crate::syscall(&mut args);
    args[6]
}

/// Register a named service using a handle fd.
///
/// The handle must be a Send IPC channel handle. Other tasks can discover
/// it via `sys_lookup_service` and then `handle_from_channel` to get their
/// own handle.
///
/// Returns `true` on success.
pub fn register_service(name: &[u8], send_fd: u32) -> bool {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::HandleRegisterService as u64;
    args[1] = name.as_ptr() as u64;
    args[2] = name.len() as u64;
    args[3] = send_fd as u64;
    crate::syscall(&mut args);
    args[6] == kernel_api_types::SVC_OK
}
