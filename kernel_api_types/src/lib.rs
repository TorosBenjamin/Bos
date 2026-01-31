#![no_std]

pub mod graphics;

#[repr(u64)]
#[derive(Clone, Copy, Debug)]
pub enum SysCallNumber {
    GetBoundingBox = 0,
    DrawIter = 1,
    FillSolid = 2,
    Exit = 3,
    Spawn = 4,
    ReadKey = 5,
    Yield = 6,
    Mmap = 7,
    Munmap = 8,
}

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
