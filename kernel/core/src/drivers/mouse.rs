use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};
use kernel_api_types::{MouseEvent, MOUSE_LEFT, MOUSE_MIDDLE, MOUSE_RIGHT};
use spin::Mutex;
use crate::task::task::{Task, TaskState};

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

/// Set when a mouse event is available (used to wake sleeping tasks)
static MOUSE_AVAILABLE: AtomicBool = AtomicBool::new(false);

/// Task sleeping on mouse input: (task_arc, cpu_kernel_id)
pub static MOUSE_WAITER: Mutex<Option<(Arc<Task>, u32)>> = Mutex::new(None);

/// 3-byte PS/2 packet accumulator
static PACKET: Mutex<[u8; 3]> = Mutex::new([0u8; 3]);
static PACKET_IDX: Mutex<u8> = Mutex::new(0);

fn push_event(event: MouseEvent) {
    MOUSE_BUFFER.lock().push(event);
    MOUSE_AVAILABLE.store(true, Ordering::Release);
    if let Some((task, cpu_id)) = MOUSE_WAITER.lock().take() {
        task.state.store(TaskState::Ready, Ordering::Release);
        crate::task::local_scheduler::add(crate::memory::cpu_local_data::get_cpu(cpu_id), task);
        let local_kernel_id = crate::memory::cpu_local_data::get_local().kernel_id;
        if cpu_id != local_kernel_id {
            let apic_id = crate::memory::cpu_local_data::local_apic_id_of(cpu_id);
            crate::apic::send_fixed_ipi(apic_id, u8::from(crate::interrupt::InterruptVector::Reschedule));
        }
    }
}

/// Try to pop a mouse event from the buffer. Returns None if empty.
pub fn try_read_mouse() -> Option<MouseEvent> {
    let result = MOUSE_BUFFER.lock().pop();
    if result.is_some() {
        if MOUSE_BUFFER.lock().count == 0 {
            MOUSE_AVAILABLE.store(false, Ordering::Release);
        }
    }
    result
}

/// Called from the mouse interrupt handler. Reads one byte from port 0x60,
/// accumulates into a 3-byte packet, and emits a MouseEvent on the 3rd byte.
pub fn on_mouse_interrupt() {
    let byte: u8 = unsafe { x86::io::inb(0x60) };

    let mut idx = PACKET_IDX.lock();
    let mut pkt = PACKET.lock();

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

        push_event(MouseEvent { dx, dy, buttons });
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
    }

    log::info!("PS/2 mouse initialized");
}
