/// IPC message type for the FAT32 filesystem server.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsMessageType {
    MapFile   = 0,
    StatFile  = 1,
    ReadDir   = 2,
    WriteFile = 3,
    CreateFile = 4,
    DeleteFile = 5,
}

/// Result codes returned by the filesystem server.
#[repr(u64)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsResult {
    Ok       = 0,
    NotFound = 1,
    IsDir    = 2,
    NotDir   = 3,
    NoSpace  = 4,
    IoError  = 5,
}

impl FsResult {
    pub fn from_u64(v: u64) -> Self {
        match v {
            0 => FsResult::Ok,
            1 => FsResult::NotFound,
            2 => FsResult::IsDir,
            3 => FsResult::NotDir,
            4 => FsResult::NoSpace,
            _ => FsResult::IoError,
        }
    }
}

/// MapFile request: client sends path, server reads the entire file into a new shared buffer.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MapFileRequest {
    pub path:     [u8; 256],
    pub path_len: u16,
}

/// MapFile response: server returns shared_buf_id and file size.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MapFileResponse {
    pub result:        u64,   // FsResult as u64
    pub shared_buf_id: u64,
    pub file_size:     u64,
}

/// StatFile request.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct StatFileRequest {
    pub path:     [u8; 256],
    pub path_len: u16,
}

/// StatFile response.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct StatFileResponse {
    pub result: u64,    // FsResult as u64
    pub size:   u64,
    pub is_dir: u8,     // 1 = directory, 0 = file
    pub _pad:   [u8; 7],
}

/// A single directory entry (64-byte name + metadata).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DirEntry {
    pub name:     [u8; 64],
    pub name_len: u8,
    pub is_dir:   u8,
    pub _pad:     [u8; 2],
    pub size:     u64,
}

/// ReadDir request.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ReadDirRequest {
    pub path:     [u8; 256],
    pub path_len: u16,
}

/// ReadDir response (up to 48 entries, fits in a 4 KB IPC message).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ReadDirResponse {
    pub result:  u64,    // FsResult as u64
    pub count:   u32,
    pub _pad:    u32,
    pub entries: [DirEntry; 48],
}

/// WriteFile request: client fills a shared buffer and the server writes it to disk.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct WriteFileRequest {
    pub path:          [u8; 256],
    pub path_len:      u16,
    pub _pad:          [u8; 6],
    pub shared_buf_id: u64,
    pub size:          u64,
}

/// WriteFile response.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct WriteFileResponse {
    pub result: u64,    // FsResult as u64
}
