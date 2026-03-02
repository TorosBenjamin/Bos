/// Client-side filesystem wrappers.
///
/// The filesystem server registers itself as the `"fatfs"` service.
/// All operations create a one-shot reply channel, send a request, and block
/// until the response arrives — the same pattern as `ulib::window`.

use core::mem;
use kernel_api_types::fs::*;
use kernel_api_types::{IPC_OK, SVC_ERR_NOT_FOUND};

// ─── Service lookup ────────────────────────────────────────────────────────────

/// Spin-yield until the `"fatfs"` service is registered.
/// Returns the send endpoint.
pub fn fs_lookup() -> u64 {
    loop {
        let ep = crate::sys_lookup_service(b"fatfs");
        if ep != SVC_ERR_NOT_FOUND {
            return ep;
        }
        crate::sys_yield();
    }
}

// ─── Request helpers ───────────────────────────────────────────────────────────

// Largest response type is ReadDirResponse (~3664 bytes); 4096 is sufficient.
const RESP_BUF_SIZE: usize = 4096;

/// Build and send a request, then await the response into `resp_buf`.
/// Returns the number of bytes received, or 0 on failure.
fn send_request_raw<Req: Sized>(
    fs_ep: u64,
    msg_type: FsMessageType,
    req: &Req,
    resp_buf: &mut [u8; RESP_BUF_SIZE],
) -> usize {
    let (our_send, our_recv) = crate::sys_channel_create(1);

    // Serialise: [type: u8][req bytes][reply_ep: u64 le]
    const MAX_MSG: usize = 1 + 512 + 8; // path requests are at most ~270 bytes
    let req_size = mem::size_of::<Req>();
    let msg_size = 1 + req_size + 8;

    if msg_size > MAX_MSG {
        crate::sys_channel_close(our_send);
        crate::sys_channel_close(our_recv);
        return 0;
    }

    let mut msg = [0u8; MAX_MSG];
    msg[0] = msg_type as u8;
    unsafe {
        core::ptr::copy_nonoverlapping(
            req as *const Req as *const u8,
            msg.as_mut_ptr().add(1),
            req_size,
        );
    }
    msg[1 + req_size..1 + req_size + 8].copy_from_slice(&our_send.to_le_bytes());

    if crate::sys_channel_send(fs_ep, &msg[..msg_size]) != IPC_OK {
        crate::sys_channel_close(our_send);
        crate::sys_channel_close(our_recv);
        return 0;
    }

    // Wait (with yield) for the response
    loop {
        let (res, len) = crate::sys_channel_recv(our_recv, resp_buf);
        if res == IPC_OK && len > 0 {
            crate::sys_channel_close(our_recv);
            return len as usize;
        }
        if res == kernel_api_types::IPC_ERR_PEER_CLOSED {
            crate::sys_channel_close(our_recv);
            return 0;
        }
        crate::sys_yield();
    }
}

/// Type-safe wrapper: sends request and reads response struct from the raw buffer.
fn send_request_and_recv<Req: Sized, Resp: Sized>(
    fs_ep: u64,
    msg_type: FsMessageType,
    req: &Req,
    resp: &mut Resp,
) -> bool {
    let mut buf = [0u8; RESP_BUF_SIZE];
    let len = send_request_raw(fs_ep, msg_type, req, &mut buf);
    if len < mem::size_of::<Resp>() {
        return false;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(buf.as_ptr(), resp as *mut Resp as *mut u8, mem::size_of::<Resp>());
    }
    true
}

fn build_path_req(path: &str) -> ([u8; 256], u16) {
    let mut buf = [0u8; 256];
    let len = path.len().min(256);
    buf[..len].copy_from_slice(path[..len].as_bytes());
    (buf, len as u16)
}

// ─── Public API ────────────────────────────────────────────────────────────────

/// Read an entire file into a new shared buffer.
///
/// Returns `Some((shared_buf_id, file_size))` on success.
/// The caller must:
///   1. `ulib::sys_map_shared_buf(id)` to get a pointer to the data.
///   2. Use the data.
///   3. `ulib::sys_destroy_shared_buf(id)` when done.
pub fn fs_map_file(fs_ep: u64, path: &str) -> Option<(u64, u64)> {
    let (path_buf, path_len) = build_path_req(path);
    let req = MapFileRequest { path: path_buf, path_len };
    let mut resp = MapFileResponse { result: FsResult::IoError as u64, shared_buf_id: u64::MAX, file_size: 0 };

    if !send_request_and_recv(fs_ep, FsMessageType::MapFile, &req, &mut resp) {
        return None;
    }
    if FsResult::from_u64(resp.result) != FsResult::Ok {
        return None;
    }
    Some((resp.shared_buf_id, resp.file_size))
}

/// Retrieve metadata for a file or directory.
pub fn fs_stat(fs_ep: u64, path: &str) -> Option<StatFileResponse> {
    let (path_buf, path_len) = build_path_req(path);
    let req = StatFileRequest { path: path_buf, path_len };
    let mut resp = StatFileResponse { result: FsResult::IoError as u64, size: 0, is_dir: 0, _pad: [0; 7] };

    if !send_request_and_recv(fs_ep, FsMessageType::StatFile, &req, &mut resp) {
        return None;
    }
    if FsResult::from_u64(resp.result) != FsResult::Ok {
        return None;
    }
    Some(resp)
}

/// List directory contents (up to 48 entries).
pub fn fs_readdir(fs_ep: u64, path: &str) -> Option<ReadDirResponse> {
    let (path_buf, path_len) = build_path_req(path);
    let req = ReadDirRequest { path: path_buf, path_len };
    let blank = kernel_api_types::fs::DirEntry { name: [0; 64], name_len: 0, is_dir: 0, _pad: [0; 2], size: 0 };
    let mut resp = ReadDirResponse {
        result: FsResult::IoError as u64, count: 0, _pad: 0, entries: [blank; 48],
    };

    if !send_request_and_recv(fs_ep, FsMessageType::ReadDir, &req, &mut resp) {
        return None;
    }
    if FsResult::from_u64(resp.result) != FsResult::Ok {
        return None;
    }
    Some(resp)
}

/// Write a file from a shared buffer.
///
/// The caller must create the shared buffer with `ulib::sys_create_shared_buf`,
/// fill it with data, then call this function.
pub fn fs_write_file(fs_ep: u64, path: &str, shared_buf_id: u64, size: u64) -> FsResult {
    let (path_buf, path_len) = build_path_req(path);
    let req = WriteFileRequest {
        path: path_buf,
        path_len,
        _pad: [0; 6],
        shared_buf_id,
        size,
    };
    let mut resp = WriteFileResponse { result: FsResult::IoError as u64 };

    if !send_request_and_recv(fs_ep, FsMessageType::WriteFile, &req, &mut resp) {
        return FsResult::IoError;
    }
    FsResult::from_u64(resp.result)
}
