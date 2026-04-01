//! Client-side filesystem wrappers.
//!
//! The filesystem server registers itself as the `"fatfs"` service.
//! All operations create a one-shot reply channel, send a request, and block
//! until the response arrives — the same pattern as `ulib::window`.
//!
//! All functions accept a `fs_fd: u32` handle (wrapping the send endpoint
//! returned by `sys_lookup_service`). Use [`fs_lookup`] to obtain one.

use core::mem;
pub use kernel_api_types::fs::FsResult;
use kernel_api_types::fs::{
    FsMessageType,
    MapFileRequest, MapFileResponse,
    StatFileRequest, StatFileResponse,
    ReadDirRequest, ReadDirResponse,
    WriteFileRequest, WriteFileResponse,
    CreateFileRequest, CreateFileResponse,
    DeleteFileRequest, DeleteFileResponse,
    MkdirRequest, MkdirResponse,
    RenameRequest, RenameResponse,
    DirEntry,
};
use kernel_api_types::SVC_ERR_NOT_FOUND;

// ─── Service lookup ────────────────────────────────────────────────────────────

/// Spin-yield until the `"fatfs"` service is registered.
/// Returns a handle fd wrapping the send endpoint.
pub fn fs_lookup() -> u32 {
    loop {
        let ep = crate::sys_lookup_service(b"fatfs");
        if ep != SVC_ERR_NOT_FOUND {
            return crate::handle::handle_from_channel(ep, 1)
                .expect("fs_lookup: handle_from_channel failed");
        }
        crate::sys_sleep_ms(1);
    }
}

// ─── Request helpers ───────────────────────────────────────────────────────────

// Largest response type is ReadDirResponse (~3664 bytes); 4096 is sufficient.
const RESP_BUF_SIZE: usize = 4096;

/// Build and send a request, then await the response into `resp_buf`.
/// Returns the number of bytes received, or 0 on failure.
fn send_request_raw<Req: Sized>(
    fs_fd: u32,
    msg_type: FsMessageType,
    req: &Req,
    resp_buf: &mut [u8; RESP_BUF_SIZE],
) -> usize {
    let (our_send, our_recv) = crate::sys_channel_create(1);

    // Wrap our_recv as a handle for reading the reply (clones the endpoint).
    let recv_fd = match crate::handle::handle_from_channel(our_recv, 0) {
        Some(fd) => fd,
        None => {
            crate::sys_channel_close(our_send);
            crate::sys_channel_close(our_recv);
            return 0;
        }
    };
    // Close original raw recv — the handle owns its clone.
    crate::sys_channel_close(our_recv);

    // Serialise: [type: u8][req bytes][reply_ep: u64 le]
    const MAX_MSG: usize = 1 + 512 + 8; // path requests are at most ~270 bytes
    let req_size = mem::size_of::<Req>();
    let msg_size = 1 + req_size + 8;

    if msg_size > MAX_MSG {
        crate::sys_channel_close(our_send);
        crate::handle::close(recv_fd);
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
    // Embed raw endpoint ID so the server can send the reply through it
    msg[1 + req_size..1 + req_size + 8].copy_from_slice(&our_send.to_le_bytes());

    if crate::handle::write(fs_fd, &msg[..msg_size]).is_none() {
        crate::sys_channel_close(our_send);
        crate::handle::close(recv_fd);
        return 0;
    }
    // Message sent — server now owns our_send and will close it after replying.

    // Wait (with yield) for the response
    loop {
        match crate::handle::read(recv_fd, resp_buf) {
            Some(len) if len > 0 => {
                crate::handle::close(recv_fd);
                return len;
            }
            Some(0) => {
                // EOF — peer closed
                crate::handle::close(recv_fd);
                return 0;
            }
            _ => {
                // No data yet — yield and retry
                crate::sys_sleep_ms(1);
            }
        }
    }
}

/// Type-safe wrapper: sends request and reads response struct from the raw buffer.
fn send_request_and_recv<Req: Sized, Resp: Sized>(
    fs_fd: u32,
    msg_type: FsMessageType,
    req: &Req,
    resp: &mut Resp,
) -> bool {
    let mut buf = [0u8; RESP_BUF_SIZE];
    let len = send_request_raw(fs_fd, msg_type, req, &mut buf);
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
    buf[..len].copy_from_slice(&path.as_bytes()[..len]);
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
pub fn fs_map_file(fs_fd: u32, path: &str) -> Option<(u64, u64)> {
    let (path_buf, path_len) = build_path_req(path);
    let req = MapFileRequest { path: path_buf, path_len };
    let mut resp = MapFileResponse { result: FsResult::IoError as u64, shared_buf_id: u64::MAX, file_size: 0 };

    if !send_request_and_recv(fs_fd, FsMessageType::MapFile, &req, &mut resp) {
        return None;
    }
    if FsResult::from_u64(resp.result) != FsResult::Ok {
        return None;
    }
    Some((resp.shared_buf_id, resp.file_size))
}

/// Retrieve metadata for a file or directory.
pub fn fs_stat(fs_fd: u32, path: &str) -> Option<StatFileResponse> {
    let (path_buf, path_len) = build_path_req(path);
    let req = StatFileRequest { path: path_buf, path_len };
    let mut resp = StatFileResponse { result: FsResult::IoError as u64, size: 0, is_dir: 0, _pad: [0; 7] };

    if !send_request_and_recv(fs_fd, FsMessageType::StatFile, &req, &mut resp) {
        return None;
    }
    if FsResult::from_u64(resp.result) != FsResult::Ok {
        return None;
    }
    Some(resp)
}

/// List directory contents (up to 48 entries).
pub fn fs_readdir(fs_fd: u32, path: &str) -> Option<ReadDirResponse> {
    let (path_buf, path_len) = build_path_req(path);
    let req = ReadDirRequest { path: path_buf, path_len };
    let blank = DirEntry { name: [0; 64], name_len: 0, is_dir: 0, _pad: [0; 2], size: 0 };
    let mut resp = ReadDirResponse {
        result: FsResult::IoError as u64, count: 0, _pad: 0, entries: [blank; 48],
    };

    if !send_request_and_recv(fs_fd, FsMessageType::ReadDir, &req, &mut resp) {
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
pub fn fs_write_file(fs_fd: u32, path: &str, shared_buf_id: u64, size: u64) -> FsResult {
    let (path_buf, path_len) = build_path_req(path);
    let req = WriteFileRequest {
        path: path_buf,
        path_len,
        _pad: [0; 6],
        shared_buf_id,
        size,
    };
    let mut resp = WriteFileResponse { result: FsResult::IoError as u64 };

    if !send_request_and_recv(fs_fd, FsMessageType::WriteFile, &req, &mut resp) {
        return FsResult::IoError;
    }
    FsResult::from_u64(resp.result)
}

/// Create an empty file at `path` (touch semantics: succeeds if it already exists).
pub fn fs_create_file(fs_fd: u32, path: &str) -> FsResult {
    let (path_buf, path_len) = build_path_req(path);
    let req = CreateFileRequest { path: path_buf, path_len };
    let mut resp = CreateFileResponse { result: FsResult::IoError as u64 };
    if !send_request_and_recv(fs_fd, FsMessageType::CreateFile, &req, &mut resp) {
        return FsResult::IoError;
    }
    FsResult::from_u64(resp.result)
}

/// Delete a file at `path`. Returns `NotFound` if not present, `IsDir` if it is a directory.
pub fn fs_rm(fs_fd: u32, path: &str) -> FsResult {
    let (path_buf, path_len) = build_path_req(path);
    let req = DeleteFileRequest { path: path_buf, path_len };
    let mut resp = DeleteFileResponse { result: FsResult::IoError as u64 };
    if !send_request_and_recv(fs_fd, FsMessageType::DeleteFile, &req, &mut resp) {
        return FsResult::IoError;
    }
    FsResult::from_u64(resp.result)
}

/// Create a new directory at `path`.
pub fn fs_mkdir(fs_fd: u32, path: &str) -> FsResult {
    let (path_buf, path_len) = build_path_req(path);
    let req = MkdirRequest { path: path_buf, path_len };
    let mut resp = MkdirResponse { result: FsResult::IoError as u64 };
    if !send_request_and_recv(fs_fd, FsMessageType::Mkdir, &req, &mut resp) {
        return FsResult::IoError;
    }
    FsResult::from_u64(resp.result)
}

/// Rename `old_path` to `new_name` (bare filename, same directory).
pub fn fs_rename(fs_fd: u32, old_path: &str, new_name: &str) -> FsResult {
    let (old_path_buf, old_path_len) = build_path_req(old_path);
    let mut new_name_buf = [0u8; 12];
    let new_name_len = new_name.len().min(12);
    new_name_buf[..new_name_len].copy_from_slice(&new_name.as_bytes()[..new_name_len]);
    let req = RenameRequest {
        old_path: old_path_buf,
        old_path_len,
        new_name: new_name_buf,
        new_name_len: new_name_len as u16,
    };
    let mut resp = RenameResponse { result: FsResult::IoError as u64 };
    if !send_request_and_recv(fs_fd, FsMessageType::Rename, &req, &mut resp) {
        return FsResult::IoError;
    }
    FsResult::from_u64(resp.result)
}
