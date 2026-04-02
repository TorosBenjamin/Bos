use core::mem;
use kernel_api_types::{MMAP_WRITE, SVC_ERR_NOT_FOUND};
use kernel_api_types::log::{
    LogEntry, LogLevel, LogMessageType, LogReadRequest, LogReadResponse, LogWriteRequest,
};

// ── Ring buffer ────────────────────────────────────────────────────────────────

const RING_CAP: usize = 64;

struct RingBuffer {
    entries: [LogEntry; RING_CAP],
    head:    usize, // next write slot
    count:   usize, // number of valid entries (saturates at RING_CAP)
}

const BLANK_ENTRY: LogEntry = LogEntry {
    level: 0, source_len: 0, msg_len: 0, _pad: [0; 4],
    timestamp: 0, source: [0; 24], message: [0; 200],
};

static mut RING: RingBuffer = RingBuffer {
    entries: [BLANK_ENTRY; RING_CAP],
    head: 0,
    count: 0,
};

impl RingBuffer {
    fn push(&mut self, e: LogEntry) {
        self.entries[self.head] = e;
        self.head = (self.head + 1) % RING_CAP;
        if self.count < RING_CAP { self.count += 1; }
    }

    /// Iterate entries from oldest to newest.
    fn iter_oldest_first(&self) -> impl Iterator<Item = &LogEntry> {
        let start = if self.count < RING_CAP {
            0
        } else {
            self.head // head is the oldest when buffer is full
        };
        (0..self.count).map(move |i| &self.entries[(start + i) % RING_CAP])
    }
}

// ── Rate limiting ──────────────────────────────────────────────────────────────

/// Max messages a single source may emit per RATE_WINDOW_MS before being throttled.
const MAX_PER_WINDOW: u32 = 20;
/// Window length in milliseconds.
const RATE_WINDOW_MS: u64 = 1000;
/// Number of distinct sources we track. Sources beyond this are never rate-limited.
const MAX_SOURCES: usize = 16;

#[derive(Clone, Copy)]
struct SourceState {
    source:       [u8; 24],
    source_len:   u8,
    _pad:         [u8; 7],
    window_start: u64, // tick at which the current window began
    count:        u32, // messages accepted in current window
    suppressed:   u32, // messages dropped in current window
}

const BLANK_SOURCE: SourceState = SourceState {
    source: [0; 24], source_len: 0, _pad: [0; 7],
    window_start: 0, count: 0, suppressed: 0,
};

static mut SOURCES: [SourceState; MAX_SOURCES] = [BLANK_SOURCE; MAX_SOURCES];
static mut SOURCE_COUNT: usize = 0;

/// Find or insert a rate-limit slot for `source`.
/// Returns `None` if the table is full (message is allowed through untracked).
fn source_slot(source: &[u8; 24], source_len: u8) -> Option<&'static mut SourceState> {
    let len = source_len as usize;
    let count = unsafe { SOURCE_COUNT };
    let slots = unsafe { (&raw mut SOURCES).as_mut().unwrap() };

    // Linear scan for existing entry
    let found = (0..count).find(|&i| {
        slots[i].source_len as usize == len && slots[i].source[..len] == source[..len]
    });
    if let Some(i) = found {
        return Some(&mut slots[i]);
    }

    // Insert new entry if space available
    if count < MAX_SOURCES {
        slots[count].source      = *source;
        slots[count].source_len  = source_len;
        slots[count].window_start = 0;
        slots[count].count       = 0;
        slots[count].suppressed  = 0;
        unsafe { SOURCE_COUNT += 1; }
        return Some(&mut slots[count]);
    }

    None // table full — allow untracked
}

/// Apply rate limiting for the incoming entry.
/// Returns `true` if the entry should be pushed to the ring, `false` if it should be dropped.
/// May push a synthetic suppression notice into `ring` when a window expires.
fn rate_check(entry: &LogEntry, ring: &mut RingBuffer) -> bool {
    let slot = match source_slot(&entry.source, entry.source_len) {
        Some(s) => s,
        None    => return true, // untracked source — always allow
    };

    let now = ulib::sys_get_ticks();

    // Check if the current window has expired
    if now.wrapping_sub(slot.window_start) >= RATE_WINDOW_MS {
        // Emit suppression notice if any messages were dropped last window
        if slot.suppressed > 0 {
            let mut msg = [0u8; 200];
            let n = write_suppression_msg(&mut msg, slot.suppressed);
            ring.push(LogEntry {
                level:      LogLevel::Warn as u8,
                source_len: slot.source_len,
                msg_len:    n as u16,
                _pad:       [0; 4],
                timestamp:  now,
                source:     slot.source,
                message:    msg,
            });
        }
        // Start new window
        slot.window_start = now;
        slot.count        = 0;
        slot.suppressed   = 0;
    }

    if slot.count >= MAX_PER_WINDOW {
        slot.suppressed += 1;
        return false;
    }

    slot.count += 1;
    true
}

/// Write "N messages suppressed" into `buf`. Returns number of bytes written.
fn write_suppression_msg(buf: &mut [u8; 200], n: u32) -> usize {
    let mut tmp = [0u8; 10];
    let digits = u32_to_decimal(n, &mut tmp);
    let suffix = b" messages suppressed";
    let total = digits.len() + suffix.len();
    let total = total.min(200);
    let dlen = digits.len().min(total);
    buf[..dlen].copy_from_slice(&digits[..dlen]);
    let slen = suffix.len().min(total - dlen);
    buf[dlen..dlen + slen].copy_from_slice(&suffix[..slen]);
    total
}

fn u32_to_decimal(n: u32, buf: &mut [u8; 10]) -> &[u8] {
    if n == 0 {
        buf[0] = b'0';
        return &buf[..1];
    }
    let mut tmp = [0u8; 10];
    let mut pos = 10usize;
    let mut v = n;
    while v > 0 {
        pos -= 1;
        tmp[pos] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    let digits = &tmp[pos..];
    let len = digits.len();
    buf[..len].copy_from_slice(digits);
    &buf[..len]
}

// ── Main server loop ───────────────────────────────────────────────────────────

const MSG_BUF_SIZE: usize = 512;

pub fn run(recv_fd: u32) -> ! {
    let ring = unsafe { (&raw mut RING).as_mut().unwrap() };
    let mut fs_fd: Option<u32> = None;
    let mut flushed_to_disk = false;

    // Record logd's own startup directly — no IPC needed since we own the ring.
    ring.push(LogEntry {
        level:      LogLevel::Info as u8,
        source_len: 4,
        msg_len:    7,
        _pad:       [0; 4],
        timestamp:  ulib::sys_get_time_ns(),
        source:     { let mut s = [0u8; 24]; s[..4].copy_from_slice(b"logd"); s },
        message:    { let mut m = [0u8; 200]; m[..7].copy_from_slice(b"started"); m },
    });

    let msg_buf = ulib::sys_mmap(MSG_BUF_SIZE as u64, MMAP_WRITE);
    if msg_buf.is_null() {
        loop { ulib::sys_sleep_ms(100); }
    }

    loop {
        // Lazily connect to fatfs once it registers, then flush early boot entries.
        if fs_fd.is_none() {
            let ep = ulib::sys_lookup_service(b"fatfs");
            if ep != SVC_ERR_NOT_FOUND {
                fs_fd = ulib::handle::handle_from_channel(ep, 0); // 0 = send (to fatfs)
            }
        }

        if !flushed_to_disk
            && let Some(fd) = fs_fd {
            flush_to_disk(ring, fd);
            flushed_to_disk = true;
        }

        let slice = unsafe { core::slice::from_raw_parts_mut(msg_buf, MSG_BUF_SIZE) };
        match ulib::handle::try_read(recv_fd, slice) {
            Some(n) if n > 0 => {
                let msg = unsafe { core::slice::from_raw_parts(msg_buf, n) };
                if msg.is_empty() { continue; }
                match msg[0] {
                    t if t == LogMessageType::Write as u8 => handle_write(msg, ring, fs_fd),
                    t if t == LogMessageType::Read  as u8 => handle_read(msg, ring),
                    _ => {}
                }
            }
            _ => { ulib::sys_sleep_ms(5); }
        }
    }
}

// ── Handlers ──────────────────────────────────────────────────────────────────

fn handle_write(msg: &[u8], ring: &mut RingBuffer, fs_fd: Option<u32>) {
    const REQ_SIZE: usize = mem::size_of::<LogWriteRequest>();
    if msg.len() < 1 + REQ_SIZE { return; }

    let req: LogWriteRequest = unsafe {
        core::ptr::read_unaligned(msg.as_ptr().add(1) as *const LogWriteRequest)
    };

    let entry = LogEntry {
        level:      req.level,
        source_len: req.source_len.min(24),
        msg_len:    req.msg_len.min(200),
        _pad:       [0; 4],
        timestamp:  ulib::sys_get_time_ns(),
        source:     req.source,
        message:    req.message,
    };

    if rate_check(&entry, ring) {
        ring.push(entry);
        if let Some(fd) = fs_fd {
            append_entry_to_disk(fd, &entry);
        }
    }
}

fn handle_read(msg: &[u8], ring: &RingBuffer) {
    const REQ_SIZE: usize = mem::size_of::<LogReadRequest>();
    if msg.len() < 1 + REQ_SIZE + 8 { return; }

    let req: LogReadRequest = unsafe {
        core::ptr::read_unaligned(msg.as_ptr().add(1) as *const LogReadRequest)
    };
    let ep_off = 1 + REQ_SIZE;
    let reply_ep = u64::from_le_bytes(msg[ep_off..ep_off + 8].try_into().unwrap_or([0; 8]));
    if reply_ep == 0 { return; }

    let count = (req.max_count as usize).min(ring.count).min(RING_CAP);

    if count == 0 {
        let resp = LogReadResponse { result: 0, shared_buf_id: u64::MAX, count: 0, _pad: [0; 4] };
        send_response(reply_ep, &resp);
        return;
    }

    let entry_size = mem::size_of::<LogEntry>();
    let (buf_id, ptr) = ulib::sys_create_shared_buf((count * entry_size) as u64);

    if ptr.is_null() || buf_id == u64::MAX {
        let resp = LogReadResponse { result: 1, shared_buf_id: u64::MAX, count: 0, _pad: [0; 4] };
        send_response(reply_ep, &resp);
        return;
    }

    for (i, entry) in ring.iter_oldest_first().take(count).enumerate() {
        unsafe {
            core::ptr::write_unaligned(ptr.add(i * entry_size) as *mut LogEntry, *entry);
        }
    }

    let resp = LogReadResponse {
        result: 0,
        shared_buf_id: buf_id,
        count: count as u32,
        _pad: [0; 4],
    };
    send_response(reply_ep, &resp);
}

fn send_response<T: Sized>(reply_ep: u64, response: &T) {
    let bytes = unsafe {
        core::slice::from_raw_parts(response as *const T as *const u8, mem::size_of::<T>())
    };
    let _ = ulib::sys_channel_send(reply_ep, bytes);
    ulib::sys_channel_close(reply_ep);
}

// ── Disk helpers ───────────────────────────────────────────────────────────────

/// Create the `logs` directory. Ignores failure (directory may already exist).
fn ensure_logs_dir(fs_fd: u32) {
    let _ = ulib::fs::fs_mkdir(fs_fd, "logs");
}

/// Append one formatted entry to `logs/<source>.log`.
fn append_entry_to_disk(fs_fd: u32, entry: &LogEntry) {
    const MAX_LINE: usize = 256;
    let (buf_id, ptr) = ulib::sys_create_shared_buf(MAX_LINE as u64);
    if ptr.is_null() || buf_id == u64::MAX { return; }

    // Format: [HH:MM:SS][LEVEL] message\n
    let mut off = 0usize;
    write_bytes(ptr, &mut off, MAX_LINE, b"[");
    write_time_hms(ptr, &mut off, MAX_LINE, entry.timestamp);
    write_bytes(ptr, &mut off, MAX_LINE, b"][");
    write_bytes(ptr, &mut off, MAX_LINE, LogLevel::from_u8(entry.level).as_str().as_bytes());
    write_bytes(ptr, &mut off, MAX_LINE, b"] ");
    write_bytes(ptr, &mut off, MAX_LINE, &entry.message[..(entry.msg_len as usize).min(200)]);
    write_bytes(ptr, &mut off, MAX_LINE, b"\n");

    if off > 0 {
        // Build path: logs/<source>.log  (max 6 + 24 + 4 = 34 bytes)
        let mut path = [0u8; 40];
        path[..5].copy_from_slice(b"logs/");
        let slen = (entry.source_len as usize).min(24);
        let name_len = if slen > 0 {
            path[5..5 + slen].copy_from_slice(&entry.source[..slen]);
            slen
        } else {
            path[5..12].copy_from_slice(b"unknown");
            7
        };
        path[5 + name_len..5 + name_len + 4].copy_from_slice(b".log");
        let path_len = 5 + name_len + 4;
        if let Ok(path_str) = core::str::from_utf8(&path[..path_len]) {
            ulib::fs::fs_append_file(fs_fd, path_str, buf_id, off as u64);
        }
    }

    ulib::sys_destroy_shared_buf(buf_id);
}

/// Flush all buffered ring entries to per-source log files under `logs/`.
/// Called once when fatfs first becomes available.
fn flush_to_disk(ring: &RingBuffer, fs_fd: u32) {
    if ring.count == 0 { return; }
    ensure_logs_dir(fs_fd);
    for entry in ring.iter_oldest_first() {
        append_entry_to_disk(fs_fd, entry);
    }
}

fn write_bytes(ptr: *mut u8, off: &mut usize, cap: usize, bytes: &[u8]) {
    let remaining = cap.saturating_sub(*off);
    let n = bytes.len().min(remaining);
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.add(*off), n); }
    *off += n;
}

fn write_time_hms(ptr: *mut u8, off: &mut usize, cap: usize, ns: u64) {
    let secs = ns / 1_000_000_000;
    let h = (secs / 3600) % 24;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    let buf = [
        b'0' + (h / 10) as u8, b'0' + (h % 10) as u8, b':',
        b'0' + (m / 10) as u8, b'0' + (m % 10) as u8, b':',
        b'0' + (s / 10) as u8, b'0' + (s % 10) as u8,
    ];
    write_bytes(ptr, off, cap, &buf);
}
