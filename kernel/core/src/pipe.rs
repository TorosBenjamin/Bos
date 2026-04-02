//! Kernel pipe — a unidirectional byte-stream between two handles.
//!
//! The pipe has a fixed 4096-byte ring buffer. Reads block when empty,
//! writes block when full. EOF is signalled when the write end is closed
//! and the buffer is drained.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;
use crate::task::task::{Task, TaskState};
use crate::task::local_scheduler::EventWaiterSlot;

type WaiterQueue = Mutex<VecDeque<(Arc<Task>, u32)>>;

pub const PIPE_BUF_SIZE: usize = 4096;

pub struct Pipe {
    inner: Mutex<PipeInner>,
    pub read_closed: AtomicBool,
    pub write_closed: AtomicBool,
    /// Tasks blocked on an empty pipe (waiting to read).
    pub read_waiters: WaiterQueue,
    /// Tasks blocked on a full pipe (waiting to write).
    pub write_waiters: WaiterQueue,
    /// For sys_wait_for_event integration (future).
    pub event_waiter: EventWaiterSlot,
}

struct PipeInner {
    buf: [u8; PIPE_BUF_SIZE],
    read_pos: usize,
    write_pos: usize,
    len: usize,
}

/// Result of a non-blocking pipe read attempt.
pub enum PipeReadResult {
    /// Read `n` bytes into the caller's buffer.
    Ok(usize),
    /// Buffer is empty — caller should block and retry.
    WouldBlock,
    /// Write end closed and buffer empty — EOF.
    Eof,
}

/// Result of a non-blocking pipe write attempt.
pub enum PipeWriteResult {
    /// Wrote `n` bytes from the caller's buffer.
    Ok(usize),
    /// Buffer is full — caller should block and retry.
    WouldBlock,
    /// Read end closed — broken pipe.
    BrokenPipe,
}

impl Pipe {
    pub fn new() -> Arc<Self> {
        Arc::new(Pipe {
            inner: Mutex::new(PipeInner {
                buf: [0u8; PIPE_BUF_SIZE],
                read_pos: 0,
                write_pos: 0,
                len: 0,
            }),
            read_closed: AtomicBool::new(false),
            write_closed: AtomicBool::new(false),
            read_waiters: Mutex::new(VecDeque::new()),
            write_waiters: Mutex::new(VecDeque::new()),
            event_waiter: Mutex::new(None),
        })
    }

    /// Try to read up to `buf.len()` bytes. Non-blocking.
    pub fn try_read(&self, buf: &mut [u8]) -> PipeReadResult {
        let mut inner = self.inner.lock();
        if inner.len == 0 {
            if self.write_closed.load(Ordering::Acquire) {
                return PipeReadResult::Eof;
            }
            return PipeReadResult::WouldBlock;
        }

        let to_read = buf.len().min(inner.len);
        for slot in &mut buf[..to_read] {
            let pos = inner.read_pos;
            *slot = inner.buf[pos];
            inner.read_pos = (pos + 1) % PIPE_BUF_SIZE;
        }
        inner.len -= to_read;
        drop(inner);

        // Wake a writer that was blocked on a full pipe.
        wake_waiter(&self.write_waiters);

        PipeReadResult::Ok(to_read)
    }

    /// Try to write up to `data.len()` bytes. Non-blocking.
    pub fn try_write(&self, data: &[u8]) -> PipeWriteResult {
        if self.read_closed.load(Ordering::Acquire) {
            return PipeWriteResult::BrokenPipe;
        }

        let mut inner = self.inner.lock();
        let available = PIPE_BUF_SIZE - inner.len;
        if available == 0 {
            return PipeWriteResult::WouldBlock;
        }

        let to_write = data.len().min(available);
        for &byte in &data[..to_write] {
            let pos = inner.write_pos;
            inner.buf[pos] = byte;
            inner.write_pos = (pos + 1) % PIPE_BUF_SIZE;
        }
        inner.len += to_write;
        drop(inner);

        // Wake a reader that was blocked on an empty pipe.
        wake_waiter(&self.read_waiters);
        // Wake event_waiter for sys_wait_for_event integration.
        crate::task::local_scheduler::try_wake_slot(&self.event_waiter);

        PipeWriteResult::Ok(to_write)
    }
}

fn wake_waiter(waiters: &WaiterQueue) {
    if let Some((task, cpu_id)) = waiters.lock().pop_front() {
        task.state.store(TaskState::Ready, Ordering::Release);
        crate::task::local_scheduler::add_front(
            crate::memory::cpu_local_data::get_cpu(cpu_id),
            task,
        );
        let local_id = crate::memory::cpu_local_data::get_local().kernel_id;
        if cpu_id != local_id {
            let apic_id = crate::memory::cpu_local_data::local_apic_id_of(cpu_id);
            crate::apic::send_fixed_ipi(
                apic_id,
                u8::from(crate::interrupt::InterruptVector::Reschedule),
            );
        }
    }
}
