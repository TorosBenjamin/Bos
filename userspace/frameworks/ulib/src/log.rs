//! Client API for the `logd` log server.
//!
//! # Usage
//!
//! ```no_run
//! ulib::log::write(LogLevel::Info, "my_service", "started successfully");
//! ulib::log::write(LogLevel::Error, "my_service", "disk full");
//! ```
//!
//! The logd endpoint is looked up lazily on first call and cached in a process-local
//! static. If logd is not yet running, the message is silently dropped (boot-time
//! messages from services that start before logd should use `sys_debug_log` instead).

use core::mem;
use core::sync::atomic::{AtomicU32, Ordering};
pub use kernel_api_types::log::LogLevel;
use kernel_api_types::log::{
    LogMessageType, LogReadRequest, LogReadResponse, LogWriteRequest,
};
use kernel_api_types::SVC_ERR_NOT_FOUND;

// u32::MAX = not yet connected
static LOGD_FD: AtomicU32 = AtomicU32::new(u32::MAX);

fn logd_fd() -> Option<u32> {
    let cached = LOGD_FD.load(Ordering::Relaxed);
    if cached != u32::MAX {
        return Some(cached);
    }
    let ep = crate::sys_lookup_service(b"logd");
    if ep == SVC_ERR_NOT_FOUND {
        return None;
    }
    let fd = crate::handle::handle_from_channel(ep, 1)?;
    LOGD_FD.store(fd, Ordering::Relaxed);
    Some(fd)
}

/// Fire-and-forget log write. Silently drops if logd is not yet running.
pub fn write(level: LogLevel, source: &str, message: &str) {
    let fd = match logd_fd() {
        Some(f) => f,
        None => return,
    };

    let mut req = LogWriteRequest {
        level:      level as u8,
        source_len: 0,
        msg_len:    0,
        source:     [0; 24],
        message:    [0; 200],
    };

    let slen = source.len().min(24);
    req.source[..slen].copy_from_slice(&source.as_bytes()[..slen]);
    req.source_len = slen as u8;

    let mlen = message.len().min(200);
    req.message[..mlen].copy_from_slice(&message.as_bytes()[..mlen]);
    req.msg_len = mlen as u16;

    let mut msg = [0u8; 1 + mem::size_of::<LogWriteRequest>()];
    msg[0] = LogMessageType::Write as u8;
    unsafe {
        core::ptr::copy_nonoverlapping(
            &req as *const LogWriteRequest as *const u8,
            msg.as_mut_ptr().add(1),
            mem::size_of::<LogWriteRequest>(),
        );
    }
    let _ = crate::handle::write(fd, &msg);
}

/// Read up to `max_count` entries from logd.
///
/// Returns `(shared_buf_id, count)` on success. The shared buffer contains
/// `count` consecutive `LogEntry` structs (oldest first).
///
/// Caller must:
/// 1. `ulib::sys_map_shared_buf(buf_id)` to get a pointer.
/// 2. Cast to `*const LogEntry` and read `count` entries.
/// 3. `ulib::sys_munmap(ptr, count * size_of::<LogEntry>())`.
/// 4. `ulib::sys_destroy_shared_buf(buf_id)`.
pub fn read(max_count: u32) -> Option<(u64, u32)> {
    let fd = logd_fd()?;

    // Create a one-shot reply channel
    let (reply_send, reply_recv) = crate::sys_channel_create(1);
    let recv_fd = crate::handle::handle_from_channel(reply_recv, 0)?;
    crate::sys_channel_close(reply_recv);

    let req = LogReadRequest { max_count, _pad: [0; 4] };

    const REQ_SIZE: usize = mem::size_of::<LogReadRequest>();
    let mut msg = [0u8; 1 + REQ_SIZE + 8];
    msg[0] = LogMessageType::Read as u8;
    unsafe {
        core::ptr::copy_nonoverlapping(
            &req as *const LogReadRequest as *const u8,
            msg.as_mut_ptr().add(1),
            REQ_SIZE,
        );
    }
    msg[1 + REQ_SIZE..1 + REQ_SIZE + 8].copy_from_slice(&reply_send.to_le_bytes());

    if crate::handle::write(fd, &msg).is_none() {
        crate::sys_channel_close(reply_send);
        crate::handle::close(recv_fd);
        return None;
    }

    // Wait for response
    let mut resp_buf = [0u8; mem::size_of::<LogReadResponse>()];
    loop {
        match crate::handle::read(recv_fd, &mut resp_buf) {
            Some(n) if n >= mem::size_of::<LogReadResponse>() => {
                crate::handle::close(recv_fd);
                break;
            }
            Some(0) => {
                crate::handle::close(recv_fd);
                return None;
            }
            _ => { crate::sys_sleep_ms(1); }
        }
    }

    let resp: LogReadResponse = unsafe {
        core::ptr::read_unaligned(resp_buf.as_ptr() as *const LogReadResponse)
    };

    if resp.result != 0 {
        if resp.shared_buf_id != u64::MAX {
            crate::sys_destroy_shared_buf(resp.shared_buf_id);
        }
        return None;
    }

    Some((resp.shared_buf_id, resp.count))
}

/// Re-export `LogEntry` so callers can interpret the shared buffer without an
/// extra `use kernel_api_types::log::LogEntry`.
pub use kernel_api_types::log::LogEntry;
