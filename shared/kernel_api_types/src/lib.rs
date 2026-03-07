#![no_std]

#[cfg(test)]
extern crate std;

pub mod fs;
pub mod graphics;
pub mod window;

#[repr(u64)]
#[derive(Clone, Copy, Debug)]
pub enum SysCallNumber {
    GetBoundingBox = 0,
    Exit = 3,
    Spawn = 4,
    ReadKey = 5,
    Yield = 6,
    Mmap = 7,
    Munmap = 8,
    ChannelCreate = 9,
    ChannelSend = 10,
    ChannelRecv = 11,
    ChannelClose = 12,
    TransferDisplay = 13,
    GetModule = 14,
    GetDisplayInfo = 15,
    DebugLog = 16,
    Waitpid = 17,
    RegisterService = 18,
    LookupService = 19,
    ReadMouse = 20,
    Shutdown = 21,
    CreateSharedBuf = 22,
    MapSharedBuf = 23,
    DestroySharedBuf = 24,
    BlockReadSectors = 25,
    BlockWriteSectors = 26,
    ThreadCreate   = 27,
    SetExitChannel = 28,
    TryReadKey     = 29,
    TryChannelRecv = 30,
    TryChannelSend = 31,
    WaitForEvent   = 32,
    SleepMs        = 33,
    SetPriority    = 34,
    GetTimeNs      = 35,
    Mprotect       = 36,
    Mremap         = 37,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Background = 0,
    Normal = 1,
    High = 2,
}

impl Priority {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Background,
            2 => Self::High,
            _ => Self::Normal,
        }
    }
}

pub const WAIT_KEYBOARD: u32 = 1;
pub const WAIT_MOUSE: u32 = 2;

pub const MAX_SERVICE_NAME_LEN: usize = 64;

pub const MOUSE_LEFT:   u8 = 1 << 0;
pub const MOUSE_RIGHT:  u8 = 1 << 1;
pub const MOUSE_MIDDLE: u8 = 1 << 2;

/// A PS/2 mouse event passed between kernel and userland.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MouseEvent {
    pub dx:        i16,
    pub dy:        i16,
    pub buttons:   u8,
    /// KEY_MOD_* bitmask of modifier keys held at event time.
    pub modifiers: u8,
}

impl MouseEvent {
    pub const EMPTY: Self = Self { dx: 0, dy: 0, buttons: 0, modifiers: 0 };
}

// IPC error codes
pub const IPC_OK: u64 = 0;
pub const IPC_ERR_INVALID_ENDPOINT: u64 = 1;
pub const IPC_ERR_WRONG_DIRECTION: u64 = 2;
pub const IPC_ERR_PEER_CLOSED: u64 = 3;
pub const IPC_ERR_CHANNEL_FULL: u64 = 4;
pub const IPC_ERR_INVALID_ARGS: u64 = 5;
pub const IPC_ERR_MSG_TOO_LARGE: u64 = 6;

// Service registry error codes
pub const SVC_OK: u64 = 0;
pub const SVC_ERR_NOT_FOUND: u64 = 10;
pub const SVC_ERR_ALREADY_REGISTERED: u64 = 11;
pub const SVC_ERR_INVALID_ARGS: u64 = 12;

pub const MMAP_WRITE: u64 = 1 << 0;
pub const MMAP_EXEC: u64 = 1 << 1;

pub const MREMAP_MAYMOVE: u64 = 1 << 0;

/// Modifier key bitmask flags carried in `KeyEvent::modifiers`.
pub const KEY_MOD_SHIFT: u8 = 1 << 0;
pub const KEY_MOD_CTRL:  u8 = 1 << 1;
pub const KEY_MOD_ALT:   u8 = 1 << 2;
pub const KEY_MOD_SUPER: u8 = 1 << 3;

/// Keyboard event types.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyEventType {
    Char = 0,
    Enter = 1,
    Backspace = 2,
    Tab = 3,
    Escape = 4,
    ArrowLeft = 5,
    ArrowRight = 6,
    ArrowUp = 7,
    ArrowDown = 8,
    F1 = 9,
    F2 = 10,
    F3 = 11,
    F4 = 12,
    F5 = 13,
    F6 = 14,
    F7 = 15,
    F8 = 16,
    F9 = 17,
    F10 = 18,
    F11 = 19,
    F12 = 20,
    Insert = 21,
    Delete = 22,
    Home = 23,
    End = 24,
    PageUp = 25,
    PageDown = 26,
}

/// A keyboard event passed between kernel and userland.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct KeyEvent {
    pub event_type: KeyEventType,
    /// The character for `Char` events, or `\0` for non-character events.
    pub character: u8,
    /// Bitmask of currently held modifier keys (KEY_MOD_* constants).
    pub modifiers: u8,
    /// `true` for key-press, `false` for key-release.
    pub pressed: bool,
}

impl KeyEvent {
    pub const EMPTY: Self = Self {
        event_type: KeyEventType::Char,
        character: 0,
        modifiers: 0,
        pressed: true,
    };

    pub const fn char(c: char) -> Self {
        Self { event_type: KeyEventType::Char, character: c as u8, modifiers: 0, pressed: true }
    }

    pub const fn enter() -> Self {
        Self { event_type: KeyEventType::Enter, character: 0, modifiers: 0, pressed: true }
    }

    pub const fn backspace() -> Self {
        Self { event_type: KeyEventType::Backspace, character: 0, modifiers: 0, pressed: true }
    }

    pub const fn tab() -> Self {
        Self { event_type: KeyEventType::Tab, character: 0, modifiers: 0, pressed: true }
    }

    pub const fn escape() -> Self {
        Self { event_type: KeyEventType::Escape, character: 0, modifiers: 0, pressed: true }
    }

    pub const fn arrow_left() -> Self {
        Self { event_type: KeyEventType::ArrowLeft, character: 0, modifiers: 0, pressed: true }
    }

    pub const fn arrow_right() -> Self {
        Self { event_type: KeyEventType::ArrowRight, character: 0, modifiers: 0, pressed: true }
    }

    pub const fn arrow_up() -> Self {
        Self { event_type: KeyEventType::ArrowUp, character: 0, modifiers: 0, pressed: true }
    }

    pub const fn arrow_down() -> Self {
        Self { event_type: KeyEventType::ArrowDown, character: 0, modifiers: 0, pressed: true }
    }

    pub const fn f_key(n: u8) -> Self {
        let event_type = match n {
            1  => KeyEventType::F1,
            2  => KeyEventType::F2,
            3  => KeyEventType::F3,
            4  => KeyEventType::F4,
            5  => KeyEventType::F5,
            6  => KeyEventType::F6,
            7  => KeyEventType::F7,
            8  => KeyEventType::F8,
            9  => KeyEventType::F9,
            10 => KeyEventType::F10,
            11 => KeyEventType::F11,
            _  => KeyEventType::F12,
        };
        Self { event_type, character: 0, modifiers: 0, pressed: true }
    }

    pub const fn insert() -> Self {
        Self { event_type: KeyEventType::Insert, character: 0, modifiers: 0, pressed: true }
    }

    pub const fn delete() -> Self {
        Self { event_type: KeyEventType::Delete, character: 0, modifiers: 0, pressed: true }
    }

    pub const fn home() -> Self {
        Self { event_type: KeyEventType::Home, character: 0, modifiers: 0, pressed: true }
    }

    pub const fn end() -> Self {
        Self { event_type: KeyEventType::End, character: 0, modifiers: 0, pressed: true }
    }

    pub const fn page_up() -> Self {
        Self { event_type: KeyEventType::PageUp, character: 0, modifiers: 0, pressed: true }
    }

    pub const fn page_down() -> Self {
        Self { event_type: KeyEventType::PageDown, character: 0, modifiers: 0, pressed: true }
    }
}
