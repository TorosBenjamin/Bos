//! Per-task handle table — maps small integers (like Unix file descriptors)
//! to kernel I/O objects (pipes, IPC channels, etc.).

use alloc::sync::Arc;
use core::sync::atomic::Ordering;
use crate::pipe::{Pipe, PipeEnd};

/// Maximum number of handles per task.
pub const MAX_HANDLES: usize = 64;

/// Which direction an IPC channel handle operates in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IpcChannelEnd {
    Send,
    Recv,
}

/// A handle to a kernel I/O object.
pub enum Handle {
    /// Read or write end of a byte-stream pipe.
    Pipe(Arc<Pipe>, PipeEnd),
    /// Wrapper around an existing IPC channel endpoint ID.
    IpcChannel(u64, IpcChannelEnd),
    /// Raw keyboard input — reads produce KeyEvent structs as bytes.
    Keyboard,
    /// Null device: reads return 0 (EOF), writes discard silently.
    Null,
}

impl Handle {
    /// Clone a handle. For pipes, increments the Arc refcount.
    /// For IPC channels, clones the endpoint (new endpoint ID on the same channel).
    /// Returns `None` if the IPC endpoint clone fails.
    pub fn try_clone(&self) -> Option<Handle> {
        match self {
            Handle::Pipe(pipe, end) => {
                if *end == PipeEnd::Write {
                    pipe.inc_write_count();
                }
                Some(Handle::Pipe(pipe.clone(), *end))
            }
            Handle::IpcChannel(ep_id, dir) => {
                let new_id = crate::ipc::clone_endpoint(*ep_id).ok()?;
                Some(Handle::IpcChannel(new_id, *dir))
            }
            Handle::Keyboard => Some(Handle::Keyboard),
            Handle::Null => Some(Handle::Null),
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        match self {
            Handle::Pipe(pipe, PipeEnd::Read) => {
                pipe.read_closed.store(true, Ordering::Release);
                // Wake any writers blocked on a full pipe so they see BrokenPipe.
                while let Some((task, cpu_id)) = pipe.write_waiters.lock().pop_front() {
                    task.state.store(crate::task::task::TaskState::Ready, Ordering::Release);
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
            Handle::Pipe(pipe, PipeEnd::Write) => {
                // Only signal EOF when the last write-end handle is closed.
                if pipe.dec_write_count() {
                    pipe.write_closed.store(true, Ordering::Release);
                    // Wake any readers blocked on an empty pipe so they see EOF.
                    while let Some((task, cpu_id)) = pipe.read_waiters.lock().pop_front() {
                        task.state.store(crate::task::task::TaskState::Ready, Ordering::Release);
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
                    crate::task::local_scheduler::try_wake_slot(&pipe.event_waiter);
                }
            }
            Handle::IpcChannel(ep_id, _) => {
                // Each handle owns its own cloned endpoint (clone-on-wrap).
                // Releasing it decrements handle_refs and closes at 0.
                let _ = crate::ipc::release_endpoint(*ep_id);
            }
            _ => {}
        }
    }
}

/// Per-task handle table with lowest-free-slot allocation.
pub struct HandleTable {
    entries: [Option<Handle>; MAX_HANDLES],
}

impl Default for HandleTable {
    fn default() -> Self { Self::new() }
}

impl HandleTable {
    pub const fn new() -> Self {
        Self {
            entries: [const { None }; MAX_HANDLES],
        }
    }

    /// Allocate at the lowest free slot. Returns the fd number, or None if full.
    pub fn alloc(&mut self, handle: Handle) -> Option<u32> {
        for (i, slot) in self.entries.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(handle);
                return Some(i as u32);
            }
        }
        None
    }

    /// Place a handle at a specific slot. Returns the displaced handle (if any).
    pub fn alloc_at(&mut self, fd: u32, handle: Handle) -> Option<Handle> {
        if (fd as usize) >= MAX_HANDLES {
            return None;
        }
        self.entries[fd as usize].replace(handle)
    }

    /// Get a reference to the handle at `fd`.
    pub fn get(&self, fd: u32) -> Option<&Handle> {
        self.entries.get(fd as usize)?.as_ref()
    }

    /// Remove and return the handle at `fd`.
    pub fn remove(&mut self, fd: u32) -> Option<Handle> {
        self.entries.get_mut(fd as usize)?.take()
    }

    /// Take all handles out (for cleanup on task exit).
    pub fn drain_all(&mut self) -> impl Iterator<Item = Handle> + '_ {
        self.entries.iter_mut().filter_map(|slot| slot.take())
    }
}
