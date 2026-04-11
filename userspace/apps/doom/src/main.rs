#![no_std]
#![no_main]
#![allow(non_upper_case_globals)]
#![allow(static_mut_refs)]
#![allow(clippy::missing_safety_doc)]
#![allow(clippy::manual_range_contains)]
#![allow(clippy::manual_is_multiple_of)]
#![allow(clippy::manual_c_str_literals)]
#![allow(clippy::slow_vector_initialization)]
#![allow(clippy::needless_range_loop)]

extern crate alloc;

use alloc::vec::Vec;
use bos_egui::egui;
use kernel_api_types::{KeyEventType, KEY_MOD_CTRL, KEY_MOD_SHIFT};

// ── Panic / entry point ──────────────────────────────────────────────────────

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop { ulib::sys_yield(); }
}

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point(_arg: u64) -> ! {
    // 16 MB heap: Doom zone needs ~6 MB, DG_ScreenBuffer ~1 MB, rest for Rust
    bos_egui::run_with_heap("doom", DoomApp::new(), 16 * 1024 * 1024)
}

// ── C declarations ───────────────────────────────────────────────────────────

unsafe extern "C" {
    fn doomgeneric_Create(argc: i32, argv: *mut *mut u8);
    fn doomgeneric_Tick();
    static mut DG_ScreenBuffer: *mut u32;
}

// ── Global state (C↔Rust bridge) ────────────────────────────────────────────

/// Ring buffer for key events to feed to DG_GetKey.
const KEY_QUEUE_CAP: usize = 64;
struct KeyQueue {
    buf:   [(bool, u8); KEY_QUEUE_CAP],
    head:  usize,
    tail:  usize,
}
impl KeyQueue {
    const fn new() -> Self {
        Self { buf: [(false, 0); KEY_QUEUE_CAP], head: 0, tail: 0 }
    }
    fn push(&mut self, pressed: bool, key: u8) {
        let next = (self.tail + 1) % KEY_QUEUE_CAP;
        if next != self.head {
            self.buf[self.tail] = (pressed, key);
            self.tail = next;
        }
    }
    fn pop(&mut self) -> Option<(bool, u8)> {
        if self.head == self.tail { return None; }
        let item = self.buf[self.head];
        self.head = (self.head + 1) % KEY_QUEUE_CAP;
        Some(item)
    }
}

static mut KEY_Q: KeyQueue = KeyQueue::new();
static mut START_TIME_NS: u64 = 0;
static mut MALLOC_COUNT: u32 = 0;
static mut TICKS_LOGGED: bool = false;
static mut PREV_CTRL: bool  = false;
static mut PREV_SHIFT: bool = false;
static mut PREV_ALT: bool   = false;

// ── App ──────────────────────────────────────────────────────────────────────

struct DoomApp {
    initialized: bool,
}

impl DoomApp {
    fn new() -> Self { Self { initialized: false } }
}

impl bos_egui::App for DoomApp {
    fn skip_bg_clear(&self) -> bool { true }

    fn update(&mut self, ctx: &egui::Context) {
        // Collect key events first
        for key in ctx.key_events() {
            process_bos_key(key);
        }

        if !self.initialized {
            self.initialized = true;
            unsafe {
                let rsp: u64;
                core::arch::asm!("mov {}, rsp", out(reg) rsp);
                ulib::sys_debug_log(rsp, 0x0001); // 0x0001 = update() entry, value=RSP

                START_TIME_NS = ulib::sys_get_time_ns();

                // Init the FS handle for fopen
                FS_FD = ulib::fs::fs_lookup();
                ulib::sys_debug_log(FS_FD as u64, 0x0002); // 0x0002 = fs_fd

                // Pre-populate stdin/stdout/stderr slots
                STDIN_PTR  = &raw mut FILE_SLOTS[0] as *mut u8;
                STDOUT_PTR = &raw mut FILE_SLOTS[1] as *mut u8;
                STDERR_PTR = &raw mut FILE_SLOTS[2] as *mut u8;
                FILE_SLOTS[0].active = true;
                FILE_SLOTS[1].active = true;
                FILE_SLOTS[2].active = true;

                // argv: ["doom", "-iwad", "DOOM1.WAD"]
                // MUST be static — doomgeneric_Create stores myargv = argv, and
                // M_CheckParm reads myargv long after this frame returns.
                static ARG0: &[u8] = b"doom\0";
                static ARG1: &[u8] = b"-iwad\0";
                static ARG2: &[u8] = b"DOOM1.WAD\0";
                static mut ARGV: [*mut u8; 4] = [core::ptr::null_mut(); 4];
                ARGV[0] = ARG0.as_ptr() as *mut u8;
                ARGV[1] = ARG1.as_ptr() as *mut u8;
                ARGV[2] = ARG2.as_ptr() as *mut u8;
                ARGV[3] = core::ptr::null_mut();
                ulib::sys_debug_log(0, 0x0003); // 0x0003 = about to call doomgeneric_Create
                doomgeneric_Create(3, ARGV.as_mut_ptr());
                ulib::sys_debug_log(0, 0x0004); // 0x0004 = doomgeneric_Create returned (we should never see this if it crashes)
            }
        } else {
            unsafe { doomgeneric_Tick(); }
        }

        // Blit DG_ScreenBuffer to the canvas, scaled to fill the window.
        egui::CentralPanel::default().show(ctx, |ui| {
            let canvas = ui.canvas();
            unsafe {
                let screen = DG_ScreenBuffer;
                if screen.is_null() { return; }
                let info = canvas.buf.info;
                let win_w = canvas.buf.width as usize;
                let win_h = canvas.buf.height as usize;

                if win_w == 1280 && win_h == 800 {
                    // Fast path: exact 2× pixel-doubling — no division, 4× fewer iterations.
                    for src_y in 0..400usize {
                        let dst_y0 = src_y * 2;
                        let dst_y1 = dst_y0 + 1;
                        for src_x in 0..640usize {
                            let pixel = *screen.add(src_y * 640 + src_x);
                            let r = ((pixel >> 16) & 0xFF) as u8;
                            let g = ((pixel >> 8)  & 0xFF) as u8;
                            let b = (pixel & 0xFF) as u8;
                            let p = info.build_pixel(r, g, b);
                            let dst_x0 = src_x * 2;
                            canvas.buf.pixels[dst_y0 * 1280 + dst_x0]     = p;
                            canvas.buf.pixels[dst_y0 * 1280 + dst_x0 + 1] = p;
                            canvas.buf.pixels[dst_y1 * 1280 + dst_x0]     = p;
                            canvas.buf.pixels[dst_y1 * 1280 + dst_x0 + 1] = p;
                        }
                    }
                } else {
                    // Generic nearest-neighbour scaling for other window sizes.
                    let win_w_i = win_w as i32;
                    let win_h_i = win_h as i32;
                    for dst_y in 0..win_h_i {
                        let src_row = (dst_y * 400 / win_h_i) as usize;
                        for dst_x in 0..win_w_i {
                            let src_col = (dst_x * 640 / win_w_i) as usize;
                            let pixel = *screen.add(src_row * 640 + src_col);
                            let r = ((pixel >> 16) & 0xFF) as u8;
                            let g = ((pixel >> 8)  & 0xFF) as u8;
                            let b = (pixel & 0xFF) as u8;
                            canvas.buf.pixels[dst_y as usize * win_w + dst_x as usize] = info.build_pixel(r, g, b);
                        }
                    }
                }
            }
        });

        // Run at ~35 tics/sec (Doom's native rate)
        bos_egui::request_timed_redraw(28);
    }
}

// ── Key translation ──────────────────────────────────────────────────────────

// Doom key codes from doomkeys.h
const KEY_RIGHTARROW: u8 = 0xae;
const KEY_LEFTARROW:  u8 = 0xac;
const KEY_UPARROW:    u8 = 0xad;
const KEY_DOWNARROW:  u8 = 0xaf;
const KEY_USE:        u8 = 0xa2;
const KEY_ESCAPE:     u8 = 27;
const KEY_ENTER:      u8 = 13;
const KEY_TAB:        u8 = 9;
const KEY_RSHIFT:     u8 = 0x80 | 0x36;
const KEY_RCTRL:      u8 = 0x80 | 0x1d;
const KEY_RALT:       u8 = 0x80 | 0x38;
const KEY_F1:  u8 = 0x80 | 0x3b;
const KEY_F2:  u8 = 0x80 | 0x3c;
const KEY_F3:  u8 = 0x80 | 0x3d;
const KEY_F4:  u8 = 0x80 | 0x3e;
const KEY_F5:  u8 = 0x80 | 0x3f;
const KEY_F6:  u8 = 0x80 | 0x40;
const KEY_F7:  u8 = 0x80 | 0x41;
const KEY_F8:  u8 = 0x80 | 0x42;
const KEY_F9:  u8 = 0x80 | 0x43;
const KEY_F10: u8 = 0x80 | 0x44;
const KEY_F11: u8 = 0x80 | 0x57;
const KEY_F12: u8 = 0x80 | 0x58;

fn process_bos_key(key: kernel_api_types::KeyEvent) {
    let pressed = key.pressed;

    // Handle modifier state changes (Ctrl, Shift, Alt)
    unsafe {
        let ctrl  = (key.modifiers & KEY_MOD_CTRL)  != 0;
        let shift = (key.modifiers & KEY_MOD_SHIFT) != 0;
        let alt   = (key.modifiers & kernel_api_types::KEY_MOD_ALT) != 0;

        if ctrl != PREV_CTRL {
            KEY_Q.push(ctrl, KEY_RCTRL);
            PREV_CTRL = ctrl;
        }
        if shift != PREV_SHIFT {
            KEY_Q.push(shift, KEY_RSHIFT);
            PREV_SHIFT = shift;
        }
        if alt != PREV_ALT {
            KEY_Q.push(alt, KEY_RALT);
            PREV_ALT = alt;
        }
    }

    let doom_key: u8 = match key.event_type {
        KeyEventType::ArrowUp    => KEY_UPARROW,
        KeyEventType::ArrowDown  => KEY_DOWNARROW,
        KeyEventType::ArrowLeft  => KEY_LEFTARROW,
        KeyEventType::ArrowRight => KEY_RIGHTARROW,
        KeyEventType::Enter   => KEY_ENTER,
        KeyEventType::Escape  => KEY_ESCAPE,
        KeyEventType::Tab     => KEY_TAB,
        KeyEventType::F1  => KEY_F1,
        KeyEventType::F2  => KEY_F2,
        KeyEventType::F3  => KEY_F3,
        KeyEventType::F4  => KEY_F4,
        KeyEventType::F5  => KEY_F5,
        KeyEventType::F6  => KEY_F6,
        KeyEventType::F7  => KEY_F7,
        KeyEventType::F8  => KEY_F8,
        KeyEventType::F9  => KEY_F9,
        KeyEventType::F10 => KEY_F10,
        KeyEventType::F11 => KEY_F11,
        KeyEventType::F12 => KEY_F12,
        KeyEventType::Char => {
            let c = key.character;
            if c == b' ' {
                KEY_USE
            } else if c == b'\r' || c == b'\n' {
                KEY_ENTER
            } else {
                // Convert to lowercase for Doom (it uppercases internally for display)
                if c >= b'A' && c <= b'Z' { c + 32 } else { c }
            }
        }
        KeyEventType::Backspace => 0x7f,
        KeyEventType::Delete => 0x80 | 0x53,
        KeyEventType::Insert => 0x80 | 0x52,
        KeyEventType::Home   => 0x80 | 0x47,
        KeyEventType::End    => 0x80 | 0x4f,
        KeyEventType::PageUp => 0x80 | 0x49,
        KeyEventType::PageDown => 0x80 | 0x51,
    };

    if doom_key != 0 {
        unsafe { KEY_Q.push(pressed, doom_key); }
    }
}

// ── Debug helper for C (I_Error messages) ────────────────────────────────────

/// Generic value logger callable from C. tag must fit in u8.
/// Usage in C: extern void __doom_log(unsigned long long val, unsigned long long tag);
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __doom_log(val: u64, tag: u64) {
    ulib::sys_debug_log(val, tag);
}

/// Called from bos_libc.c's vfprintf to emit error messages to the debug log.
/// Logs up to 4 consecutive 8-byte chunks so the full message is visible.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __doom_debug_str(s: *const u8, len: usize) {
    let bytes = unsafe { core::slice::from_raw_parts(s, len.min(32)) };
    for chunk in bytes.chunks(8) {
        let mut tag: u64 = 0;
        for (i, &b) in chunk.iter().enumerate() {
            tag |= (b as u64) << (i * 8);
        }
        ulib::sys_debug_log(tag, 0xE9); // 0xE9 = error string chunk
    }
}

// ── Platform functions (called from C) ───────────────────────────────────────

#[unsafe(no_mangle)]
extern "C" fn DG_Init() {
    ulib::sys_debug_log(0, 0x10); // 0x10 = DG_Init called
}

#[unsafe(no_mangle)]
extern "C" fn DG_DrawFrame() {

    // DG_ScreenBuffer already contains the rendered frame; nothing to do here.
    // The Rust update() reads it directly after doomgeneric_Tick() returns.
}

#[unsafe(no_mangle)]
extern "C" fn DG_SleepMs(ms: u32) {
    ulib::sys_sleep_ms(ms as u64);
}

#[unsafe(no_mangle)]
extern "C" fn DG_GetTicksMs() -> u32 {
    let now = ulib::sys_get_time_ns();
    let elapsed = unsafe { now.saturating_sub(START_TIME_NS) };
    // Log first call — signals doom has reached D_DoomLoop/TryRunTics
    unsafe {
        if !TICKS_LOGGED {
            TICKS_LOGGED = true;
            ulib::sys_debug_log((elapsed / 1_000_000) as u64, 0xB2);
        }
    }
    (elapsed / 1_000_000) as u32
}

#[unsafe(no_mangle)]
extern "C" fn DG_GetKey(pressed: *mut i32, key: *mut u8) -> i32 {
    unsafe {
        match KEY_Q.pop() {
            Some((p, k)) => {
                *pressed = if p { 1 } else { 0 };
                *key = k;
                1
            }
            None => 0,
        }
    }
}

#[unsafe(no_mangle)]
extern "C" fn DG_SetWindowTitle(_title: *const u8) {}

// ── libc stubs ───────────────────────────────────────────────────────────────
// Memory

#[unsafe(no_mangle)]
unsafe extern "C" fn malloc(size: usize) -> *mut u8 {
    if size == 0 { return core::ptr::null_mut(); }
    unsafe {
        MALLOC_COUNT += 1;
        // Log every 200th call so we can track init progress without spamming
        if MALLOC_COUNT % 200 == 0 {
            ulib::sys_debug_log(MALLOC_COUNT as u64, 0xCC); // 0xCC = malloc count milestone
        }
        if size > 512 * 1024 {
            ulib::sys_debug_log(size as u64, 0xAA); // 0xAA = large malloc, value=size
        }
        let layout = core::alloc::Layout::from_size_align(size + core::mem::size_of::<usize>(), 8).unwrap();
        let ptr = alloc::alloc::alloc(layout);
        if ptr.is_null() {
            ulib::sys_debug_log(size as u64, 0xAB); // 0xAB = malloc FAILED
            return core::ptr::null_mut();
        }
        // Store allocation size for free/realloc
        *(ptr as *mut usize) = size;
        ptr.add(core::mem::size_of::<usize>())
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn free(ptr: *mut u8) {
    if ptr.is_null() { return; }
    unsafe {
        let base = ptr.sub(core::mem::size_of::<usize>());
        let size = *(base as *const usize);
        let layout = core::alloc::Layout::from_size_align(size + core::mem::size_of::<usize>(), 8).unwrap();
        alloc::alloc::dealloc(base, layout);
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn realloc(ptr: *mut u8, new_size: usize) -> *mut u8 {
    unsafe {
        if ptr.is_null() { return malloc(new_size); }
        if new_size == 0 { free(ptr); return core::ptr::null_mut(); }
        let base = ptr.sub(core::mem::size_of::<usize>());
        let old_size = *(base as *const usize);
        let new_ptr = malloc(new_size);
        if new_ptr.is_null() { return core::ptr::null_mut(); }
        let copy_len = old_size.min(new_size);
        core::ptr::copy_nonoverlapping(ptr, new_ptr, copy_len);
        free(ptr);
        new_ptr
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn calloc(count: usize, size: usize) -> *mut u8 {
    let total = count.saturating_mul(size);
    unsafe {
        let ptr = malloc(total);
        if !ptr.is_null() {
            core::ptr::write_bytes(ptr, 0, total);
        }
        ptr
    }
}

// String functions

#[unsafe(no_mangle)]
unsafe extern "C" fn strlen(s: *const u8) -> usize {
    let mut n = 0;
    unsafe {
        while *s.add(n) != 0 { n += 1; }
    }
    n
}

#[unsafe(no_mangle)]
unsafe extern "C" fn strcpy(dst: *mut u8, src: *const u8) -> *mut u8 {
    let mut i = 0;
    unsafe {
        loop {
            let c = *src.add(i);
            *dst.add(i) = c;
            if c == 0 { break; }
            i += 1;
        }
    }
    dst
}

#[unsafe(no_mangle)]
unsafe extern "C" fn strncpy(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    unsafe {
        for i in 0..n {
            let c = *src.add(i);
            *dst.add(i) = c;
            if c == 0 {
                for j in (i + 1)..n { *dst.add(j) = 0; }
                return dst;
            }
        }
    }
    dst
}

#[unsafe(no_mangle)]
unsafe extern "C" fn strcmp(s1: *const u8, s2: *const u8) -> i32 {
    let mut i = 0;
    unsafe {
        loop {
            let a = *s1.add(i);
            let b = *s2.add(i);
            if a != b { return (a as i32) - (b as i32); }
            if a == 0 { return 0; }
            i += 1;
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn strncmp(s1: *const u8, s2: *const u8, n: usize) -> i32 {
    unsafe {
        for i in 0..n {
            let a = *s1.add(i);
            let b = *s2.add(i);
            if a != b { return (a as i32) - (b as i32); }
            if a == 0 { return 0; }
        }
    }
    0
}

unsafe fn ascii_lower(c: u8) -> u8 {
    if c >= b'A' && c <= b'Z' { c + 32 } else { c }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn strcasecmp(s1: *const u8, s2: *const u8) -> i32 {
    let mut i = 0;
    unsafe {
        loop {
            let a = ascii_lower(*s1.add(i));
            let b = ascii_lower(*s2.add(i));
            if a != b { return (a as i32) - (b as i32); }
            if a == 0 { return 0; }
            i += 1;
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn strncasecmp(s1: *const u8, s2: *const u8, n: usize) -> i32 {
    unsafe {
        for i in 0..n {
            let a = ascii_lower(*s1.add(i));
            let b = ascii_lower(*s2.add(i));
            if a != b { return (a as i32) - (b as i32); }
            if a == 0 { return 0; }
        }
    }
    0
}

#[unsafe(no_mangle)]
unsafe extern "C" fn strcat(dst: *mut u8, src: *const u8) -> *mut u8 {
    unsafe {
        let len = strlen(dst as *const u8);
        strcpy(dst.add(len), src);
    }
    dst
}

#[unsafe(no_mangle)]
unsafe extern "C" fn strncat(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    unsafe {
        let len = strlen(dst as *const u8);
        for i in 0..n {
            let c = *src.add(i);
            *dst.add(len + i) = c;
            if c == 0 { return dst; }
        }
        *dst.add(len + n) = 0;
    }
    dst
}

#[unsafe(no_mangle)]
unsafe extern "C" fn strchr(s: *const u8, c: i32) -> *mut u8 {
    let ch = c as u8;
    let mut i = 0;
    unsafe {
        loop {
            let b = *s.add(i);
            if b == ch { return s.add(i) as *mut u8; }
            if b == 0  { return core::ptr::null_mut(); }
            i += 1;
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn strrchr(s: *const u8, c: i32) -> *mut u8 {
    let ch = c as u8;
    unsafe {
        let len = strlen(s);
        let mut i = len as isize;
        while i >= 0 {
            if *s.add(i as usize) == ch { return s.add(i as usize) as *mut u8; }
            i -= 1;
        }
    }
    core::ptr::null_mut()
}

#[unsafe(no_mangle)]
unsafe extern "C" fn strdup(s: *const u8) -> *mut u8 {
    unsafe {
        let len = strlen(s);
        let dst = malloc(len + 1);
        if dst.is_null() { return core::ptr::null_mut(); }
        core::ptr::copy_nonoverlapping(s, dst, len + 1);
        dst
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn strstr(haystack: *const u8, needle: *const u8) -> *mut u8 {
    unsafe {
        let nlen = strlen(needle);
        if nlen == 0 { return haystack as *mut u8; }
        let hlen = strlen(haystack);
        if hlen < nlen { return core::ptr::null_mut(); }
        for i in 0..=(hlen - nlen) {
            if strncmp(haystack.add(i), needle, nlen) == 0 {
                return haystack.add(i) as *mut u8;
            }
        }
    }
    core::ptr::null_mut()
}

static mut STRTOK_STATE: *mut u8 = core::ptr::null_mut();

#[unsafe(no_mangle)]
unsafe extern "C" fn strtok(s: *mut u8, delim: *const u8) -> *mut u8 {
    unsafe {
        let ptr = if !s.is_null() { s } else { STRTOK_STATE };
        if ptr.is_null() { return core::ptr::null_mut(); }
        // Skip leading delimiters
        let mut cur = ptr;
        loop {
            let c = *cur;
            if c == 0 { STRTOK_STATE = core::ptr::null_mut(); return core::ptr::null_mut(); }
            if strchr(delim, c as i32).is_null() { break; }
            cur = cur.add(1);
        }
        let start = cur;
        loop {
            let c = *cur;
            if c == 0 { STRTOK_STATE = cur; break; }
            if !strchr(delim, c as i32).is_null() {
                *cur = 0;
                STRTOK_STATE = cur.add(1);
                break;
            }
            cur = cur.add(1);
        }
        start
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn strerror(_errnum: i32) -> *mut u8 {
    b"error\0".as_ptr() as *mut u8
}

// Memory operations — these may clash with compiler_builtins; use weak linkage
// to let the compiler's builtins win if present.
// Actually, use regular no_mangle — compiler_builtins provides weak symbols.

#[unsafe(no_mangle)]
unsafe extern "C" fn memset(s: *mut u8, c: i32, n: usize) -> *mut u8 {
    let byte = c as u8;
    unsafe {
        for i in 0..n { *s.add(i) = byte; }
    }
    s
}

#[unsafe(no_mangle)]
unsafe extern "C" fn memcpy(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    // Do NOT call copy_nonoverlapping here — the compiler lowers it to a `call memcpy`
    // instruction, which would recurse back into this function and exhaust the stack.
    unsafe {
        for i in 0..n { *dst.add(i) = *src.add(i); }
    }
    dst
}

#[unsafe(no_mangle)]
unsafe extern "C" fn memmove(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    // Do NOT call core::ptr::copy here — same recursion risk as memcpy.
    unsafe {
        if (dst as usize) <= (src as usize) || (dst as usize) >= (src as usize).wrapping_add(n) {
            for i in 0..n { *dst.add(i) = *src.add(i); }
        } else {
            let mut i = n;
            while i > 0 { i -= 1; *dst.add(i) = *src.add(i); }
        }
    }
    dst
}

#[unsafe(no_mangle)]
unsafe extern "C" fn memcmp(s1: *const u8, s2: *const u8, n: usize) -> i32 {
    unsafe {
        for i in 0..n {
            let a = *s1.add(i);
            let b = *s2.add(i);
            if a != b { return (a as i32) - (b as i32); }
        }
    }
    0
}

// Char classification

#[unsafe(no_mangle)] extern "C" fn toupper(c: i32) -> i32 {
    if c >= b'a' as i32 && c <= b'z' as i32 { c - 32 } else { c }
}
#[unsafe(no_mangle)] extern "C" fn tolower(c: i32) -> i32 {
    if c >= b'A' as i32 && c <= b'Z' as i32 { c + 32 } else { c }
}
#[unsafe(no_mangle)] extern "C" fn isdigit(c: i32) -> i32 {
    if (c >= b'0' as i32) && (c <= b'9' as i32) { 1 } else { 0 }
}
#[unsafe(no_mangle)] extern "C" fn isxdigit(c: i32) -> i32 {
    let ok = (c >= b'0' as i32 && c <= b'9' as i32)
          || (c >= b'a' as i32 && c <= b'f' as i32)
          || (c >= b'A' as i32 && c <= b'F' as i32);
    if ok { 1 } else { 0 }
}
#[unsafe(no_mangle)] extern "C" fn isalpha(c: i32) -> i32 {
    let ok = (c >= b'a' as i32 && c <= b'z' as i32) || (c >= b'A' as i32 && c <= b'Z' as i32);
    if ok { 1 } else { 0 }
}
#[unsafe(no_mangle)] extern "C" fn isalnum(c: i32) -> i32 {
    if isalpha(c) != 0 || isdigit(c) != 0 { 1 } else { 0 }
}
#[unsafe(no_mangle)] extern "C" fn isspace(c: i32) -> i32 {
    let ok = c == b' ' as i32 || c == b'\t' as i32 || c == b'\n' as i32
          || c == b'\r' as i32 || c == 0x0C || c == 0x0B;
    if ok { 1 } else { 0 }
}
#[unsafe(no_mangle)] extern "C" fn isprint(c: i32) -> i32 {
    if c >= 0x20 && c < 0x7F { 1 } else { 0 }
}
#[unsafe(no_mangle)] extern "C" fn isupper(c: i32) -> i32 {
    if c >= b'A' as i32 && c <= b'Z' as i32 { 1 } else { 0 }
}
#[unsafe(no_mangle)] extern "C" fn islower(c: i32) -> i32 {
    if c >= b'a' as i32 && c <= b'z' as i32 { 1 } else { 0 }
}
#[unsafe(no_mangle)] extern "C" fn iscntrl(c: i32) -> i32 {
    if c < 0x20 || c == 0x7F { 1 } else { 0 }
}
#[unsafe(no_mangle)] extern "C" fn ispunct(c: i32) -> i32 {
    if isprint(c) != 0 && isalnum(c) == 0 && c != b' ' as i32 { 1 } else { 0 }
}

// Numeric conversions

#[unsafe(no_mangle)]
unsafe extern "C" fn atoi(s: *const u8) -> i32 {
    unsafe { strtol_impl(s, core::ptr::null_mut(), 10) as i32 }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn atol(s: *const u8) -> i64 {
    unsafe { strtol_impl(s, core::ptr::null_mut(), 10) }
}

unsafe fn strtol_impl(s: *const u8, endptr: *mut *mut u8, base: i32) -> i64 {
    unsafe {
        let mut p = s;
        while isspace(*p as i32) != 0 { p = p.add(1); }
        let neg = if *p == b'-' { p = p.add(1); true }
                  else if *p == b'+' { p = p.add(1); false }
                  else { false };
        let base_val: u64 = if base == 0 {
            if *p == b'0' {
                p = p.add(1);
                if *p == b'x' || *p == b'X' { p = p.add(1); 16 } else { 8 }
            } else { 10 }
        } else { base as u64 };
        let mut val: u64 = 0;
        loop {
            let c = *p;
            let digit: u64 = if c >= b'0' && c <= b'9' { (c - b'0') as u64 }
                else if c >= b'a' && c <= b'z' { (c - b'a' + 10) as u64 }
                else if c >= b'A' && c <= b'Z' { (c - b'A' + 10) as u64 }
                else { break };
            if digit >= base_val { break; }
            val = val.wrapping_mul(base_val).wrapping_add(digit);
            p = p.add(1);
        }
        if !endptr.is_null() { *endptr = p as *mut u8; }
        if neg { -(val as i64) } else { val as i64 }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn strtol(s: *const u8, endptr: *mut *mut u8, base: i32) -> i64 {
    unsafe { strtol_impl(s, endptr, base) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn strtoul(s: *const u8, endptr: *mut *mut u8, base: i32) -> u64 {
    unsafe { strtol_impl(s, endptr, base) as u64 }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn atof(s: *const u8) -> f64 {
    // Very minimal: handle integer part and one decimal place
    unsafe {
        let mut p = s;
        while isspace(*p as i32) != 0 { p = p.add(1); }
        let neg = if *p == b'-' { p = p.add(1); true } else if *p == b'+' { p = p.add(1); false } else { false };
        let mut int_part: f64 = 0.0;
        while *p >= b'0' && *p <= b'9' { int_part = int_part * 10.0 + (*p - b'0') as f64; p = p.add(1); }
        let mut frac: f64 = 0.0;
        if *p == b'.' {
            p = p.add(1);
            let mut div = 10.0f64;
            while *p >= b'0' && *p <= b'9' {
                frac += (*p - b'0') as f64 / div;
                div *= 10.0;
                p = p.add(1);
            }
        }
        let val = int_part + frac;
        if neg { -val } else { val }
    }
}

#[unsafe(no_mangle)]
extern "C" fn abs(n: i32) -> i32 { if n < 0 { -n } else { n } }

#[unsafe(no_mangle)]
extern "C" fn labs(n: i64) -> i64 { if n < 0 { -n } else { n } }

// Misc

#[unsafe(no_mangle)]
extern "C" fn exit(status: i32) -> ! {
    ulib::sys_debug_log(status as u64, 0xDE); // 0xDE = exit() called, value=status
    ulib::sys_exit(status as u64)
}

static mut ATEXIT_FUNCS: [Option<unsafe extern "C" fn()>; 16] = [None; 16];
static mut ATEXIT_COUNT: usize = 0;

#[unsafe(no_mangle)]
unsafe extern "C" fn atexit(func: unsafe extern "C" fn()) -> i32 {
    unsafe {
        if ATEXIT_COUNT < 16 {
            ATEXIT_FUNCS[ATEXIT_COUNT] = Some(func);
            ATEXIT_COUNT += 1;
            0
        } else { -1 }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn getenv(_name: *const u8) -> *mut u8 {
    core::ptr::null_mut()
}

#[unsafe(no_mangle)]
unsafe extern "C" fn system(_cmd: *const u8) -> i32 { -1 }

#[unsafe(no_mangle)]
unsafe extern "C" fn remove(_path: *const u8) -> i32 { -1 }

#[unsafe(no_mangle)]
unsafe extern "C" fn rename(_old: *const u8, _new: *const u8) -> i32 { -1 }

#[unsafe(no_mangle)]
unsafe extern "C" fn mkdir(_path: *const u8, _mode: u32) -> i32 { 0 }

#[unsafe(no_mangle)]
unsafe extern "C" fn stat(_path: *const u8, _buf: *mut u8) -> i32 { -1 }

#[unsafe(no_mangle)]
unsafe extern "C" fn isatty(_fd: i32) -> i32 { 0 }

#[unsafe(no_mangle)]
extern "C" fn rand() -> i32 {
    // Simple LCG — Doom uses its own RNG for gameplay; this is for edge cases
    static mut SEED: u64 = 12345;
    unsafe {
        SEED = SEED.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((SEED >> 33) & 0x7FFF_FFFF) as i32
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn srand(_seed: u32) {}

// qsort — simple insertion sort (good enough for small Doom sprite arrays)
#[unsafe(no_mangle)]
unsafe extern "C" fn qsort(
    base: *mut u8,
    count: usize,
    size: usize,
    cmp: unsafe extern "C" fn(*const u8, *const u8) -> i32,
) {
    if count <= 1 { return; }
    let mut tmp = Vec::with_capacity(size);
    tmp.resize(size, 0u8);
    unsafe {
        for i in 1..count {
            core::ptr::copy_nonoverlapping(base.add(i * size), tmp.as_mut_ptr(), size);
            let mut j = i;
            while j > 0 && cmp(base.add((j-1)*size), tmp.as_ptr()) > 0 {
                core::ptr::copy_nonoverlapping(base.add((j-1)*size), base.add(j*size), size);
                j -= 1;
            }
            core::ptr::copy_nonoverlapping(tmp.as_ptr(), base.add(j*size), size);
        }
    }
}

// errno global
#[unsafe(no_mangle)]
pub static mut errno: i32 = 0;

// ── File I/O (MemFile) ────────────────────────────────────────────────────────

const MAX_OPEN_FILES: usize = 16;

// Simple file cache: keeps the shared_buf_id + mapped pointer alive so that
// repeated fopen of the same file (e.g. doom opens DOOM1.WAD twice — once in
// M_FileExists and once in W_AddFile) doesn't re-read the WAD from disk.
const FILE_CACHE_CAP: usize = 4;

struct FileCacheEntry {
    name: [u8; 64],
    name_len: usize,
    shared_buf_id: u64,
    data: *const u8,
    size: usize,
}

impl FileCacheEntry {
    const fn zero() -> Self {
        Self { name: [0; 64], name_len: 0, shared_buf_id: 0, data: core::ptr::null(), size: 0 }
    }
}

unsafe impl Send for FileCacheEntry {}
unsafe impl Sync for FileCacheEntry {}

static mut FILE_CACHE: [FileCacheEntry; FILE_CACHE_CAP] = [
    FileCacheEntry::zero(), FileCacheEntry::zero(),
    FileCacheEntry::zero(), FileCacheEntry::zero(),
];

// Look up a path in the cache; returns (data, size, shared_buf_id) or None.
unsafe fn cache_lookup(path_bytes: &[u8]) -> Option<(*const u8, usize, u64)> {
    unsafe {
        for entry in FILE_CACHE.iter() {
            if entry.data.is_null() { continue; }
            if entry.name_len == path_bytes.len() && entry.name[..entry.name_len] == *path_bytes {
                return Some((entry.data, entry.size, entry.shared_buf_id));
            }
        }
    }
    None
}

// Store a mapping in the cache. Silently no-ops if cache is full.
unsafe fn cache_insert(path_bytes: &[u8], shared_buf_id: u64, data: *const u8, size: usize) {
    unsafe {
        for entry in FILE_CACHE.iter_mut() {
            if entry.data.is_null() {
                let len = path_bytes.len().min(64);
                entry.name[..len].copy_from_slice(&path_bytes[..len]);
                entry.name_len = len;
                entry.shared_buf_id = shared_buf_id;
                entry.data = data;
                entry.size = size;
                return;
            }
        }
    }
}

struct MemFile {
    active: bool,
    data: *const u8,
    size: usize,
    cursor: usize,
    shared_buf_id: u64,
    // If true, this slot points into cached data — don't destroy the shared buf on fclose.
    from_cache: bool,
}

unsafe impl Send for MemFile {}
unsafe impl Sync for MemFile {}

impl MemFile {
    const fn zero() -> Self {
        Self { active: false, data: core::ptr::null(), size: 0, cursor: 0, shared_buf_id: 0, from_cache: false }
    }
}

static mut FILE_SLOTS: [MemFile; MAX_OPEN_FILES] = [
    MemFile::zero(), MemFile::zero(), MemFile::zero(), MemFile::zero(),
    MemFile::zero(), MemFile::zero(), MemFile::zero(), MemFile::zero(),
    MemFile::zero(), MemFile::zero(), MemFile::zero(), MemFile::zero(),
    MemFile::zero(), MemFile::zero(), MemFile::zero(), MemFile::zero(),
];

// fs_fd obtained at init time
static mut FS_FD: u32 = 0;

// stdin/stdout/stderr pointers (exported as C globals)
#[unsafe(no_mangle)] pub static mut stdin:  *mut u8 = core::ptr::null_mut();
#[unsafe(no_mangle)] pub static mut stdout: *mut u8 = core::ptr::null_mut();
#[unsafe(no_mangle)] pub static mut stderr: *mut u8 = core::ptr::null_mut();
static mut STDIN_PTR:  *mut u8 = core::ptr::null_mut();
static mut STDOUT_PTR: *mut u8 = core::ptr::null_mut();
static mut STDERR_PTR: *mut u8 = core::ptr::null_mut();

unsafe fn alloc_file_slot() -> Option<*mut MemFile> {
    unsafe {
        // Slots 0-2 are pre-allocated for stdin/stdout/stderr
        for i in 3..MAX_OPEN_FILES {
            if !FILE_SLOTS[i].active {
                FILE_SLOTS[i].active = true;
                FILE_SLOTS[i].data = core::ptr::null();
                FILE_SLOTS[i].size = 0;
                FILE_SLOTS[i].cursor = 0;
                FILE_SLOTS[i].shared_buf_id = 0;
                FILE_SLOTS[i].from_cache = false;
                return Some(&raw mut FILE_SLOTS[i]);
            }
        }
    }
    None
}

#[unsafe(no_mangle)]
unsafe extern "C" fn fopen(path: *const u8, mode: *const u8) -> *mut u8 {
    unsafe {
        // Only support read mode
        let mode_byte = *mode;
        if mode_byte != b'r' {
            // Write mode — return a dummy writable slot so Doom doesn't crash
            // (config writes are silently discarded)
            return match alloc_file_slot() {
                Some(slot) => {
                    // Leave data=null, writes go nowhere
                    slot as *mut u8
                }
                None => core::ptr::null_mut(),
            };
        }

        let slot = match alloc_file_slot() {
            Some(s) => s,
            None => return core::ptr::null_mut(),
        };

        // Build path string from C pointer
        let len = strlen(path);
        let path_bytes = core::slice::from_raw_parts(path, len);
        let path_str = core::str::from_utf8_unchecked(path_bytes);

        // Debug: log first 8 bytes of filename as a tag
        let mut name_tag: u64 = 0;
        for (i, &b) in path_bytes.iter().take(8).enumerate() {
            name_tag |= (b as u64) << (i * 8);
        }
        ulib::sys_debug_log(name_tag, 0xF0); // 0xF0 = fopen called, value=first 8 chars of path

        // Check cache first — doom opens DOOM1.WAD twice (M_FileExists + W_AddFile)
        if let Some((cached_data, cached_size, cached_buf_id)) = cache_lookup(path_bytes) {
            (*slot).data = cached_data;
            (*slot).size = cached_size;
            (*slot).cursor = 0;
            (*slot).shared_buf_id = cached_buf_id;
            (*slot).from_cache = true;
            ulib::sys_debug_log(cached_size as u64, 0xF1);
            return slot as *mut u8;
        }

        // Cache miss: map the file via the FS server
        match ulib::fs::fs_map_file(FS_FD, path_str) {
            Some((buf_id, file_size)) => {
                let data_ptr = ulib::sys_map_shared_buf(buf_id);
                (*slot).data = data_ptr;
                (*slot).size = file_size as usize;
                (*slot).cursor = 0;
                (*slot).shared_buf_id = buf_id;
                // Populate the cache for future opens of the same file.
                // The cache now owns the shared buffer lifetime — mark from_cache=true
                // so that fclose on THIS slot also doesn't destroy the buffer.
                cache_insert(path_bytes, buf_id, data_ptr, file_size as usize);
                (*slot).from_cache = true;
                ulib::sys_debug_log(file_size, 0xF1); // 0xF1 = fopen succeeded, value=file_size
                // Log first 8 bytes of the mapped data so we can verify WAD magic + numlumps
                if !data_ptr.is_null() && file_size >= 8 {
                    let mut hdr: u64 = 0;
                    for i in 0..8usize {
                        hdr |= (*data_ptr.add(i) as u64) << (i * 8);
                    }
                    ulib::sys_debug_log(hdr, 0xF3); // 0xF3 = first 8 bytes of file data
                }
                slot as *mut u8
            }
            None => {
                // File not found
                ulib::sys_debug_log(name_tag, 0xF2); // 0xF2 = fopen FAILED
                (*slot).active = false;
                core::ptr::null_mut()
            }
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn fclose(stream: *mut u8) -> i32 {
    unsafe {
        if stream.is_null() { return -1; }
        let slot = &mut *(stream as *mut MemFile);
        if !slot.active { return -1; }
        // Only destroy the shared buf if this slot owns it (not a cache hit).
        if slot.shared_buf_id != 0 && !slot.from_cache {
            ulib::sys_destroy_shared_buf(slot.shared_buf_id);
            slot.shared_buf_id = 0;
            slot.data = core::ptr::null();
        }
        slot.active = false;
        0
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn fread(ptr: *mut u8, size: usize, count: usize, stream: *mut u8) -> usize {
    unsafe {
        if stream.is_null() || ptr.is_null() { return 0; }
        let slot = &mut *(stream as *mut MemFile);
        if slot.data.is_null() { return 0; }
        let total = size.saturating_mul(count);
        let available = slot.size.saturating_sub(slot.cursor);
        let to_read = total.min(available);
        if to_read == 0 { return 0; }
        core::ptr::copy_nonoverlapping(slot.data.add(slot.cursor), ptr, to_read);
        slot.cursor += to_read;
        to_read / size.max(1)
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn fwrite(_ptr: *const u8, _size: usize, count: usize, _stream: *mut u8) -> usize {
    count // pretend success (writes discarded)
}

#[unsafe(no_mangle)]
unsafe extern "C" fn fseek(stream: *mut u8, offset: i64, whence: i32) -> i32 {
    unsafe {
        if stream.is_null() { return -1; }
        let slot = &mut *(stream as *mut MemFile);
        if slot.data.is_null() { return 0; } // allow seeks on null-data files (write mode)
        let new_cursor: i64 = match whence {
            0 /* SEEK_SET */ => offset,
            1 /* SEEK_CUR */ => slot.cursor as i64 + offset,
            2 /* SEEK_END */ => slot.size as i64 + offset,
            _ => return -1,
        };
        if new_cursor < 0 { return -1; }
        slot.cursor = new_cursor as usize;
        0
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn ftell(stream: *mut u8) -> i64 {
    unsafe {
        if stream.is_null() { return -1; }
        let slot = &*(stream as *const MemFile);
        slot.cursor as i64
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn fflush(_stream: *mut u8) -> i32 { 0 }

#[unsafe(no_mangle)]
unsafe extern "C" fn fileno(_stream: *mut u8) -> i32 { 0 }
