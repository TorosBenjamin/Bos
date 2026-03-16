use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};
use kernel_api_types::KeyEvent;
use spin::Mutex;
use crate::task::task::{Task, TaskState};

/// PS/2 Set 1 scancode-to-ASCII lookup table (unshifted)
static NORMAL: &[u8] = &[
    0, 27, b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'8', b'9', b'0', b'-', b'=', b'\x08',
    b'\t', b'q', b'w', b'e', b'r', b't', b'y', b'u', b'i', b'o', b'p', b'[', b']', b'\n',
    0, b'a', b's', b'd', b'f', b'g', b'h', b'j', b'k', b'l', b';', b'\'', b'`',
    0, b'\\', b'z', b'x', b'c', b'v', b'b', b'n', b'm', b',', b'.', b'/', 0, b'*',
    0, b' ',
];

/// PS/2 Set 1 scancode-to-ASCII lookup table (shifted)
static SHIFTED: &[u8] = &[
    0, 27, b'!', b'@', b'#', b'$', b'%', b'^', b'&', b'*', b'(', b')', b'_', b'+', b'\x08',
    b'\t', b'Q', b'W', b'E', b'R', b'T', b'Y', b'U', b'I', b'O', b'P', b'{', b'}', b'\n',
    0, b'A', b'S', b'D', b'F', b'G', b'H', b'J', b'K', b'L', b':', b'"', b'~',
    0, b'|', b'Z', b'X', b'C', b'V', b'B', b'N', b'M', b'<', b'>', b'?', 0, b'*',
    0, b' ',
];

const KEY_BUFFER_SIZE: usize = 64;

struct KeyBuffer {
    buffer: [KeyEvent; KEY_BUFFER_SIZE],
    head: usize,
    tail: usize,
    count: usize,
}

impl KeyBuffer {
    const fn new() -> Self {
        Self {
            buffer: [KeyEvent::EMPTY; KEY_BUFFER_SIZE],
            head: 0,
            tail: 0,
            count: 0,
        }
    }

    fn push(&mut self, event: KeyEvent) {
        self.buffer[self.tail] = event;
        self.tail = (self.tail + 1) % KEY_BUFFER_SIZE;
        if self.count == KEY_BUFFER_SIZE {
            self.head = (self.head + 1) % KEY_BUFFER_SIZE;
        } else {
            self.count += 1;
        }
    }

    fn pop(&mut self) -> Option<KeyEvent> {
        if self.count == 0 {
            return None;
        }
        let event = self.buffer[self.head];
        self.head = (self.head + 1) % KEY_BUFFER_SIZE;
        self.count -= 1;
        Some(event)
    }
}

static KEY_BUFFER: Mutex<KeyBuffer> = Mutex::new(KeyBuffer::new());

/// Set when a key event is available (used to wake sleeping tasks)
static KEY_AVAILABLE: AtomicBool = AtomicBool::new(false);

/// Task sleeping on keyboard input: (task_arc, cpu_kernel_id)
pub static KEYBOARD_WAITER: Mutex<Option<(Arc<Task>, u32)>> = Mutex::new(None);

/// Waiter slot for sys_wait_for_event; woken by CAS when a key event arrives.
pub static KEYBOARD_EVENT_WAITER: crate::task::local_scheduler::EventWaiterSlot = Mutex::new(None);

// Keyboard state
static SHIFT_PRESSED: Mutex<bool> = Mutex::new(false);
static CAPSLOCK_ON:   Mutex<bool> = Mutex::new(false);
static EXTENDED:      Mutex<bool> = Mutex::new(false);
static ALT_PRESSED:   Mutex<bool> = Mutex::new(false);
static CTRL_PRESSED:  Mutex<bool> = Mutex::new(false);
static SUPER_PRESSED: Mutex<bool> = Mutex::new(false);

pub fn current_modifiers() -> u8 {
    use kernel_api_types::{KEY_MOD_SHIFT, KEY_MOD_CTRL, KEY_MOD_ALT, KEY_MOD_SUPER};
    let mut m = 0u8;
    if *SHIFT_PRESSED.lock() { m |= KEY_MOD_SHIFT; }
    if *CTRL_PRESSED.lock()  { m |= KEY_MOD_CTRL;  }
    if *ALT_PRESSED.lock()   { m |= KEY_MOD_ALT;   }
    if *SUPER_PRESSED.lock() { m |= KEY_MOD_SUPER;  }
    m
}

fn scancode_to_ascii(code: u8, uppercase: bool) -> Option<char> {
    let table = if uppercase { SHIFTED } else { NORMAL };
    if (code as usize) < table.len() {
        let c = table[code as usize];
        if c != 0 {
            return Some(c as char);
        }
    }
    None
}

/// Process a raw PS/2 scancode and push key events to the buffer.
///
/// This is also used by tests to feed scancodes without doing port I/O.
pub fn handle_scancode(scancode: u8) {
    // E0 prefix for extended keys
    if scancode == 0xE0 {
        *EXTENDED.lock() = true;
        return;
    }

    let released = scancode & 0x80 != 0;
    let code = scancode & 0x7F;
    let pressed = !released;

    let mut extended = EXTENDED.lock();
    let is_extended = *extended;
    *extended = false;
    drop(extended);

    // Extended keys: modifiers first, then arrow keys
    if is_extended {
        // Right Alt / AltGr
        if code == 0x38 { *ALT_PRESSED.lock() = pressed; return; }
        // Right Ctrl
        if code == 0x1D { *CTRL_PRESSED.lock() = pressed; return; }
        // Left Super (0x5B) / Right Super (0x5C)
        if code == 0x5B || code == 0x5C { *SUPER_PRESSED.lock() = pressed; return; }

        let mods = current_modifiers();
        let event = match code {
            0x48 => Some(KeyEvent::arrow_up()),
            0x50 => Some(KeyEvent::arrow_down()),
            0x4B => Some(KeyEvent::arrow_left()),
            0x4D => Some(KeyEvent::arrow_right()),
            0x47 => Some(KeyEvent::home()),
            0x4F => Some(KeyEvent::end()),
            0x49 => Some(KeyEvent::page_up()),
            0x51 => Some(KeyEvent::page_down()),
            0x52 => Some(KeyEvent::insert()),
            0x53 => Some(KeyEvent::delete()),
            _ => None,
        };
        if let Some(mut ev) = event {
            ev.modifiers = mods;
            ev.pressed = pressed;
            push_event(ev);
        }
        return;
    }

    // Left Shift / Right Shift
    if code == 0x2A || code == 0x36 {
        *SHIFT_PRESSED.lock() = pressed;
        return;
    }

    // Left Alt
    if code == 0x38 {
        *ALT_PRESSED.lock() = pressed;
        return;
    }

    // Left Ctrl
    if code == 0x1D {
        *CTRL_PRESSED.lock() = pressed;
        return;
    }

    // Caps lock (toggle on press only)
    if code == 0x3A && pressed {
        let mut caps = CAPSLOCK_ON.lock();
        *caps = !*caps;
        return;
    }

    let mods = current_modifiers();

    // Special keys
    let event = match code {
        0x01 => Some(KeyEvent::escape()),
        0x0E => Some(KeyEvent::backspace()),
        0x0F => Some(KeyEvent::tab()),
        0x1C => Some(KeyEvent::enter()),
        0x3B => Some(KeyEvent::f_key(1)),
        0x3C => Some(KeyEvent::f_key(2)),
        0x3D => Some(KeyEvent::f_key(3)),
        0x3E => Some(KeyEvent::f_key(4)),
        0x3F => Some(KeyEvent::f_key(5)),
        0x40 => Some(KeyEvent::f_key(6)),
        0x41 => Some(KeyEvent::f_key(7)),
        0x42 => Some(KeyEvent::f_key(8)),
        0x43 => Some(KeyEvent::f_key(9)),
        0x44 => Some(KeyEvent::f_key(10)),
        0x57 => Some(KeyEvent::f_key(11)),
        0x58 => Some(KeyEvent::f_key(12)),
        _ => {
            let shift = *SHIFT_PRESSED.lock();
            let caps = *CAPSLOCK_ON.lock();
            let uppercase = shift ^ caps;
            scancode_to_ascii(code, uppercase).map(KeyEvent::char)
        }
    };

    if let Some(mut ev) = event {
        ev.modifiers = mods;
        ev.pressed = pressed;
        push_event(ev);
    }
}

fn push_event(event: KeyEvent) {
    KEY_BUFFER.lock().push(event);
    KEY_AVAILABLE.store(true, Ordering::Release);
    if let Some((task, cpu_id)) = KEYBOARD_WAITER.lock().take() {
        task.state.store(TaskState::Ready, Ordering::Release);
        crate::task::local_scheduler::add(crate::memory::cpu_local_data::get_cpu(cpu_id), task);
        let local_kernel_id = crate::memory::cpu_local_data::get_local().kernel_id;
        if cpu_id != local_kernel_id {
            let apic_id = crate::memory::cpu_local_data::local_apic_id_of(cpu_id);
            crate::apic::send_fixed_ipi(apic_id, u8::from(crate::interrupt::InterruptVector::Reschedule));
        }
    }
    // Wake any task sleeping in sys_wait_for_event watching keyboard
    crate::task::local_scheduler::try_wake_slot(&KEYBOARD_EVENT_WAITER);
}

/// Try to pop a key event from the buffer. Returns None if empty.
pub fn try_read_key() -> Option<KeyEvent> {
    let result = KEY_BUFFER.lock().pop();
    if result.is_some() {
        // Check if buffer is now empty
        if KEY_BUFFER.lock().count == 0 {
            KEY_AVAILABLE.store(false, Ordering::Release);
        }
    }
    result
}

/// Check if a key event is available without consuming it.
pub fn has_key() -> bool {
    KEY_AVAILABLE.load(Ordering::Acquire)
}

/// Called from the keyboard interrupt handler.
pub fn on_keyboard_interrupt() {
    let scancode: u8 = unsafe { x86::io::inb(0x60) };
    handle_scancode(scancode);
}

/// Reset all keyboard state (buffer, shift, capslock, extended).
/// Used by tests to ensure a clean state between test cases.
pub fn reset() {
    let mut buf = KEY_BUFFER.lock();
    *buf = KeyBuffer::new();
    *SHIFT_PRESSED.lock() = false;
    *CAPSLOCK_ON.lock() = false;
    *EXTENDED.lock() = false;
    *ALT_PRESSED.lock() = false;
    *CTRL_PRESSED.lock() = false;
    *SUPER_PRESSED.lock() = false;
    KEY_AVAILABLE.store(false, Ordering::Release);
}
