use kernel_api_types::{MouseEvent, MOUSE_LEFT, MOUSE_MIDDLE, MOUSE_RIGHT};
use spin::Mutex;
use core::sync::atomic::{AtomicBool, Ordering};

const MOUSE_BUFFER_SIZE: usize = 64;

struct MouseBuffer {
    buffer: [MouseEvent; MOUSE_BUFFER_SIZE],
    head: usize,
    tail: usize,
    count: usize,
}

impl MouseBuffer {
    const fn new() -> Self {
        Self {
            buffer: [MouseEvent::EMPTY; MOUSE_BUFFER_SIZE],
            head: 0,
            tail: 0,
            count: 0,
        }
    }

    fn push(&mut self, event: MouseEvent) {
        if self.count < MOUSE_BUFFER_SIZE {
            self.buffer[self.tail] = event;
            self.tail = (self.tail + 1) % MOUSE_BUFFER_SIZE;
            self.count += 1;
        }
    }

    fn pop(&mut self) -> Option<MouseEvent> {
        if self.count == 0 {
            return None;
        }
        let event = self.buffer[self.head];
        self.head = (self.head + 1) % MOUSE_BUFFER_SIZE;
        self.count -= 1;
        Some(event)
    }
}

static MOUSE_BUFFER: Mutex<MouseBuffer> = Mutex::new(MouseBuffer::new());

/// True whenever MOUSE_BUFFER is non-empty. Set in push_event, cleared in try_read_mouse.
static MOUSE_AVAILABLE: AtomicBool = AtomicBool::new(false);

/// Waiter slot for sys_wait_for_event; woken by CAS when a mouse event arrives.
pub static MOUSE_WAITER: crate::task::local_scheduler::EventWaiterSlot = Mutex::new(None);

/// 3-byte PS/2 packet accumulator
static PACKET: Mutex<[u8; 3]> = Mutex::new([0u8; 3]);
static PACKET_IDX: Mutex<u8> = Mutex::new(0);

fn push_event(event: MouseEvent) {
    MOUSE_BUFFER.lock().push(event);
    MOUSE_AVAILABLE.store(true, Ordering::Release);
    crate::task::local_scheduler::try_wake_slot(&MOUSE_WAITER);
}

/// Try to pop a mouse event from the buffer. Returns None if empty.
/// Clears MOUSE_AVAILABLE when the buffer becomes empty after the pop.
pub fn try_read_mouse() -> Option<MouseEvent> {
    let mut buf = MOUSE_BUFFER.lock();
    let result = buf.pop();
    if buf.count == 0 {
        MOUSE_AVAILABLE.store(false, Ordering::Release);
    }
    result
}

/// Returns true if at least one mouse event is buffered (non-consuming check).
pub fn has_mouse() -> bool {
    MOUSE_AVAILABLE.load(Ordering::Acquire)
}

/// Called from the mouse interrupt handler. Reads one byte from port 0x60,
/// accumulates into a 3-byte packet, and emits a MouseEvent on the 3rd byte.
pub fn on_mouse_interrupt() {
    let byte: u8 = unsafe { x86::io::inb(0x60) };

    let mut idx = PACKET_IDX.lock();
    let mut pkt = PACKET.lock();

    // PS/2 status byte always has bit 3 set. If we receive a byte at position 0
    // that doesn't have bit 3 set, it's not a valid status byte — we're out of sync.
    // Discard it and wait for a real status byte to re-synchronise.
    if *idx == 0 && (byte & 0x08 == 0) {
        return;
    }

    pkt[*idx as usize] = byte;
    *idx += 1;

    if *idx == 3 {
        *idx = 0;

        let status = pkt[0];
        let raw_dx = pkt[1];
        let raw_dy = pkt[2];

        // Discard packet if overflow bits are set
        if status & 0xC0 != 0 {
            return;
        }

        let dx = (raw_dx as i16) | (if status & 0x10 != 0 { -256i16 } else { 0 });
        let dy = (raw_dy as i16) | (if status & 0x20 != 0 { -256i16 } else { 0 });
        let dy = -dy; // PS/2 Y is inverted; positive = down in screen coords

        let mut buttons: u8 = 0;
        if status & 0x01 != 0 { buttons |= MOUSE_LEFT; }
        if status & 0x02 != 0 { buttons |= MOUSE_RIGHT; }
        if status & 0x04 != 0 { buttons |= MOUSE_MIDDLE; }

        drop(pkt);
        drop(idx);

        push_event(MouseEvent { dx, dy, buttons, modifiers: super::keyboard::current_modifiers() });
    }
}

fn ps2_wait_write() {
    loop {
        let status: u8 = unsafe { x86::io::inb(0x64) };
        if status & 0x02 == 0 {
            break;
        }
        core::hint::spin_loop();
    }
}

fn ps2_wait_read() {
    loop {
        let status: u8 = unsafe { x86::io::inb(0x64) };
        if status & 0x01 != 0 {
            break;
        }
        core::hint::spin_loop();
    }
}

/// Initialize the PS/2 mouse: enable aux port, enable IRQ12, set stream mode.
pub fn init() {
    unsafe {
        // Enable aux port
        ps2_wait_write();
        x86::io::outb(0x64, 0xA8);

        // Request controller config byte
        ps2_wait_write();
        x86::io::outb(0x64, 0x20);
        ps2_wait_read();
        let mut ccb: u8 = x86::io::inb(0x60);

        // Enable IRQ12 (bit 1), enable aux clock (clear bit 5)
        ccb |= 0x02;
        ccb &= !0x20;

        // Write config byte back
        ps2_wait_write();
        x86::io::outb(0x64, 0x60);
        ps2_wait_write();
        x86::io::outb(0x60, ccb);

        // Send 0xF4 (Enable streaming) to the mouse device
        ps2_wait_write();
        x86::io::outb(0x64, 0xD4);
        ps2_wait_write();
        x86::io::outb(0x60, 0xF4);

        // The mouse responds to 0xF4 with ACK (0xFA). We must drain it here
        // via polling, before interrupts are enabled (STI fires later in main.rs).
        // If we don't, the pending IRQ12 fires on the first STI and on_mouse_interrupt()
        // reads 0xFA as pkt[0], permanently misaligning the 3-byte packet accumulator.
        ps2_wait_read();
        let ack = x86::io::inb(0x60);
        log::info!("PS/2 mouse: drained ACK byte {:#04x}", ack);
    }

    log::info!("PS/2 mouse initialized");
}
