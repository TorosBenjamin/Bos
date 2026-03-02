use core::mem;
use kernel_api_types::fs::*;
use kernel_api_types::{IPC_OK, MMAP_WRITE};

use crate::fat32::{BlockDev, Entry, Fat32};

// ─── Production disk backend ───────────────────────────────────────────────────

struct SysDisk;

impl BlockDev for SysDisk {
    fn read(&mut self, lba: u64, buf: &mut [u8; 512]) -> bool {
        ulib::sys_block_read_sectors(lba, 1, buf) != 0
    }
    fn write(&mut self, lba: u64, buf: &[u8; 512]) -> bool {
        ulib::sys_block_write_sectors(lba, 1, buf) != 0
    }
}

// ─── Server loop ───────────────────────────────────────────────────────────────

const MAX_MSG_SIZE: usize = 4096;

pub fn run(recv_ep: u64) -> ! {
    let mut fs = match Fat32::mount(SysDisk) {
        Some(f) => {
            ulib::sys_debug_log(1, 0xFA32_0000); // "fatfs: mounted"
            f
        }
        None => {
            ulib::sys_debug_log(0, 0xFA32_DEAD); // "fatfs: mount failed"
            loop { ulib::sys_yield(); }
        }
    };

    let msg_buf = ulib::sys_mmap(MAX_MSG_SIZE as u64, MMAP_WRITE);
    if msg_buf.is_null() {
        loop { ulib::sys_yield(); }
    }

    loop {
        let msg_slice = unsafe { core::slice::from_raw_parts_mut(msg_buf, MAX_MSG_SIZE) };
        let (result, bytes_read) = ulib::sys_channel_recv(recv_ep, msg_slice);

        if result != IPC_OK || bytes_read == 0 {
            ulib::sys_yield();
            continue;
        }

        let msg = unsafe { core::slice::from_raw_parts(msg_buf, bytes_read as usize) };
        if msg.is_empty() {
            continue;
        }

        match msg[0] {
            t if t == FsMessageType::MapFile as u8   => handle_map_file(&mut fs, msg),
            t if t == FsMessageType::StatFile as u8  => handle_stat_file(&mut fs, msg),
            t if t == FsMessageType::ReadDir as u8   => handle_read_dir(&mut fs, msg),
            t if t == FsMessageType::WriteFile as u8 => handle_write_file(&mut fs, msg),
            _ => {}
        }
    }
}

// ─── Helpers ───────────────────────────────────────────────────────────────────

fn extract_reply_ep(msg: &[u8], after_offset: usize) -> Option<u64> {
    if msg.len() < after_offset + 8 {
        return None;
    }
    Some(u64::from_le_bytes(msg[after_offset..after_offset + 8].try_into().ok()?))
}

fn path_str<'a>(path: &'a [u8; 256], path_len: u16) -> Option<&'a str> {
    let len = path_len as usize;
    if len > 256 { return None; }
    core::str::from_utf8(&path[..len]).ok()
}

fn send_response<T: Sized>(reply_ep: u64, response: &T) {
    let bytes = unsafe {
        core::slice::from_raw_parts(response as *const T as *const u8, mem::size_of::<T>())
    };
    let _ = ulib::sys_channel_send(reply_ep, bytes);
    ulib::sys_channel_close(reply_ep);
}

// ─── Handlers ──────────────────────────────────────────────────────────────────

fn handle_map_file(fs: &mut Fat32<SysDisk>, msg: &[u8]) {
    const REQ: usize = mem::size_of::<MapFileRequest>();
    let reply_ep = match extract_reply_ep(msg, 1 + REQ) {
        Some(ep) => ep,
        None => return,
    };

    let err_resp = |result: FsResult| MapFileResponse {
        result: result as u64,
        shared_buf_id: u64::MAX,
        file_size: 0,
    };

    if msg.len() < 1 + REQ {
        send_response(reply_ep, &err_resp(FsResult::IoError));
        return;
    }
    let req: MapFileRequest = unsafe {
        core::ptr::read_unaligned(msg.as_ptr().add(1) as *const MapFileRequest)
    };
    let path = match path_str(&req.path, req.path_len) {
        Some(p) => p,
        None => { send_response(reply_ep, &err_resp(FsResult::IoError)); return; }
    };

    let entry = match fs.lookup(path) {
        Some(e) if !e.is_dir => e,
        Some(_) => { send_response(reply_ep, &err_resp(FsResult::IsDir)); return; }
        None    => { send_response(reply_ep, &err_resp(FsResult::NotFound)); return; }
    };

    let file_size = entry.size as u64;
    let (buf_id, ptr) = ulib::sys_create_shared_buf(file_size.max(1));
    if ptr.is_null() || buf_id == u64::MAX {
        send_response(reply_ep, &err_resp(FsResult::NoSpace));
        return;
    }

    let actual = fs.read_file(entry.cluster, entry.size, ptr);
    if actual < entry.size as usize {
        ulib::sys_destroy_shared_buf(buf_id);
        send_response(reply_ep, &err_resp(FsResult::IoError));
        return;
    }

    send_response(reply_ep, &MapFileResponse {
        result: FsResult::Ok as u64,
        shared_buf_id: buf_id,
        file_size,
    });
}

fn handle_stat_file(fs: &mut Fat32<SysDisk>, msg: &[u8]) {
    const REQ: usize = mem::size_of::<StatFileRequest>();
    let reply_ep = match extract_reply_ep(msg, 1 + REQ) {
        Some(ep) => ep,
        None => return,
    };

    let err_resp = |result: FsResult| StatFileResponse {
        result: result as u64, size: 0, is_dir: 0, _pad: [0; 7],
    };

    if msg.len() < 1 + REQ {
        send_response(reply_ep, &err_resp(FsResult::IoError));
        return;
    }
    let req: StatFileRequest = unsafe {
        core::ptr::read_unaligned(msg.as_ptr().add(1) as *const StatFileRequest)
    };
    let path = match path_str(&req.path, req.path_len) {
        Some(p) => p,
        None => { send_response(reply_ep, &err_resp(FsResult::IoError)); return; }
    };

    match fs.lookup(path) {
        Some(e) => send_response(reply_ep, &StatFileResponse {
            result: FsResult::Ok as u64,
            size: e.size as u64,
            is_dir: e.is_dir as u8,
            _pad: [0; 7],
        }),
        None => send_response(reply_ep, &err_resp(FsResult::NotFound)),
    }
}

fn handle_read_dir(fs: &mut Fat32<SysDisk>, msg: &[u8]) {
    const REQ: usize = mem::size_of::<ReadDirRequest>();
    let reply_ep = match extract_reply_ep(msg, 1 + REQ) {
        Some(ep) => ep,
        None => return,
    };

    let blank = kernel_api_types::fs::DirEntry {
        name: [0; 64], name_len: 0, is_dir: 0, _pad: [0; 2], size: 0,
    };

    let err_resp = |result: FsResult| ReadDirResponse {
        result: result as u64, count: 0, _pad: 0, entries: [blank; 48],
    };

    if msg.len() < 1 + REQ {
        send_response(reply_ep, &err_resp(FsResult::IoError));
        return;
    }
    let req: ReadDirRequest = unsafe {
        core::ptr::read_unaligned(msg.as_ptr().add(1) as *const ReadDirRequest)
    };
    let path = match path_str(&req.path, req.path_len) {
        Some(p) => p,
        None => { send_response(reply_ep, &err_resp(FsResult::IoError)); return; }
    };

    let dir_cluster = match fs.lookup(path) {
        Some(e) if e.is_dir => e.cluster,
        Some(_) => { send_response(reply_ep, &err_resp(FsResult::NotDir)); return; }
        None    => { send_response(reply_ep, &err_resp(FsResult::NotFound)); return; }
    };

    let mut fat_entries: [Entry; 48] = core::array::from_fn(|_| Entry {
        cluster: 0, size: 0, is_dir: false,
        name: [0; 12], name_len: 0,
    });
    let count = fs.read_dir(dir_cluster, &mut fat_entries);

    let mut resp = ReadDirResponse {
        result: FsResult::Ok as u64,
        count: count as u32,
        _pad: 0,
        entries: [blank; 48],
    };

    for (i, fe) in fat_entries[..count].iter().enumerate() {
        let name_len = fe.name_len.min(63);
        resp.entries[i].name[..name_len].copy_from_slice(&fe.name[..name_len]);
        resp.entries[i].name_len = name_len as u8;
        resp.entries[i].is_dir = fe.is_dir as u8;
        resp.entries[i].size = fe.size as u64;
    }

    send_response(reply_ep, &resp);
}

fn handle_write_file(fs: &mut Fat32<SysDisk>, msg: &[u8]) {
    const REQ: usize = mem::size_of::<WriteFileRequest>();
    let reply_ep = match extract_reply_ep(msg, 1 + REQ) {
        Some(ep) => ep,
        None => return,
    };

    let err_resp = |result: FsResult| WriteFileResponse { result: result as u64 };

    if msg.len() < 1 + REQ {
        send_response(reply_ep, &err_resp(FsResult::IoError));
        return;
    }
    let req: WriteFileRequest = unsafe {
        core::ptr::read_unaligned(msg.as_ptr().add(1) as *const WriteFileRequest)
    };
    let path = match path_str(&req.path, req.path_len) {
        Some(p) => p,
        None => { send_response(reply_ep, &err_resp(FsResult::IoError)); return; }
    };

    if path.contains('/') {
        send_response(reply_ep, &err_resp(FsResult::IoError));
        return;
    }

    let ptr = ulib::sys_map_shared_buf(req.shared_buf_id);
    if ptr.is_null() {
        send_response(reply_ep, &err_resp(FsResult::IoError));
        return;
    }

    let data = unsafe { core::slice::from_raw_parts(ptr as *const u8, req.size as usize) };
    let ok = fs.write_file(path, data);
    send_response(reply_ep, &err_resp(if ok { FsResult::Ok } else { FsResult::IoError }));
}
