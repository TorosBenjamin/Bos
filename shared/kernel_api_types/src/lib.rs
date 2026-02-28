#![no_std]

#[cfg(test)]
extern crate std;

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
}

pub const MAX_SERVICE_NAME_LEN: usize = 64;

pub const MOUSE_LEFT:   u8 = 1 << 0;
pub const MOUSE_RIGHT:  u8 = 1 << 1;
pub const MOUSE_MIDDLE: u8 = 1 << 2;

/// A PS/2 mouse event passed between kernel and userland.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MouseEvent {
    pub dx:      i16,
    pub dy:      i16,
    pub buttons: u8,
}

impl MouseEvent {
    pub const EMPTY: Self = Self { dx: 0, dy: 0, buttons: 0 };
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
}

/// A keyboard event passed between kernel and userland.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct KeyEvent {
    pub event_type: KeyEventType,
    /// The character for `Char` events, or `\0` for non-character events.
    pub character: u8,
}

impl KeyEvent {
    pub const EMPTY: Self = Self {
        event_type: KeyEventType::Char,
        character: 0,
    };

    pub const fn char(c: char) -> Self {
        Self {
            event_type: KeyEventType::Char,
            character: c as u8,
        }
    }

    pub const fn enter() -> Self {
        Self { event_type: KeyEventType::Enter, character: 0 }
    }

    pub const fn backspace() -> Self {
        Self { event_type: KeyEventType::Backspace, character: 0 }
    }

    pub const fn tab() -> Self {
        Self { event_type: KeyEventType::Tab, character: 0 }
    }

    pub const fn escape() -> Self {
        Self { event_type: KeyEventType::Escape, character: 0 }
    }

    pub const fn arrow_left() -> Self {
        Self { event_type: KeyEventType::ArrowLeft, character: 0 }
    }

    pub const fn arrow_right() -> Self {
        Self { event_type: KeyEventType::ArrowRight, character: 0 }
    }

    pub const fn arrow_up() -> Self {
        Self { event_type: KeyEventType::ArrowUp, character: 0 }
    }

    pub const fn arrow_down() -> Self {
        Self { event_type: KeyEventType::ArrowDown, character: 0 }
    }
}
