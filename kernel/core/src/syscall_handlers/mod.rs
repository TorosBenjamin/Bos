mod task;
mod memory;
mod ipc;
mod graphics;
mod misc;
mod service;
mod pci;
mod ioport;
mod event;

pub use task::{sys_exit, sys_yield, sys_spawn, sys_waitpid, sys_thread_create, sys_set_exit_channel, sys_sleep_ms, sys_set_priority, sys_set_fault_ep, sys_wait_task_ready};
pub(crate) use task::kill_from_exception;
pub use memory::{sys_mmap, sys_munmap, sys_mprotect, sys_mremap, sys_create_shared_buf, sys_map_shared_buf, sys_destroy_shared_buf, sys_alloc_dma};
pub use ipc::{sys_channel_create, sys_channel_send, sys_channel_recv, sys_channel_close, sys_try_channel_recv, sys_try_channel_send};
pub use graphics::{sys_get_bounding_box, sys_get_display_info, sys_transfer_display};
pub use misc::{sys_debug_log, sys_read_key, sys_try_read_key, sys_read_mouse, sys_get_module, sys_shutdown, sys_get_time_ns};
pub use service::{sys_register_service, sys_lookup_service};
pub use pci::{sys_pci_config_read, sys_pci_config_write, sys_map_pci_bar};
pub use ioport::{sys_ioport_read, sys_ioport_write};
pub use event::{sys_wait_for_event, check_timeout_waiters};

use alloc::sync::Arc;
use crate::memory::cpu_local_data::{get_cpu, get_local, local_apic_id_of};
use crate::task::local_scheduler;
use crate::task::task::{Task, TaskKind, TaskState};
use core::sync::atomic::Ordering;

/// RAII guard that holds a read lock on the current task's VMA lock.
///
/// While this guard is alive, no other thread can unmap or remap user-space
/// pages (`sys_munmap`/`sys_mprotect`/`sys_mremap` acquire the write lock).
/// This closes the TOCTOU window between pointer validation and kernel use.
pub(crate) struct UserPtrGuard {
    // Fields are dropped in declaration order:
    //   1. `_guard` — releases the RwLock read lock
    //   2. `_task`  — decrements the Arc refcount
    //
    // SAFETY: The `'static` lifetime is a transmuted lie. It is sound because:
    //   - The guard borrows from `_task.vma_lock`, which lives as long as the Arc.
    //   - `_guard` is dropped before `_task` (declaration order), so the borrow
    //     is always released while the RwLock is still alive.
    _guard: spin::RwLockReadGuard<'static, ()>,
    _task: Arc<Task>,
}

/// Validates that `[ptr, ptr+size)` is within the current user task's
/// allocated virtual address space, prefaults any lazy pages, and returns
/// a guard that prevents concurrent unmapping while the kernel uses the pointer.
///
/// Returns `None` if the pointer is invalid.
fn validate_user_ptr(ptr: u64, size: u64) -> Option<UserPtrGuard> {
    if ptr == 0 || size == 0 {
        return None;
    }
    let end = ptr.checked_add(size)?;
    if ptr < crate::consts::USER_MIN || end > crate::consts::USER_MAX + 1 {
        return None;
    }
    let cpu = get_local();
    let rq = cpu.run_queue.get().unwrap().lock();
    let task = match &rq.current_task {
        Some(t) if t.kind == TaskKind::User => t.clone(),
        _ => return None,
    };
    drop(rq);

    // Acquire the VMA read lock BEFORE checking VMAs. This prevents a
    // concurrent sys_munmap (which takes the write lock) from removing the
    // VMA or unmapping pages between our check and the caller's use.
    let guard = task.vma_lock.read();

    {
        let inner = task.inner.lock();
        if !crate::memory::user_vaddr::is_user_vaddr_valid_range(
            &inner.user_vmas,
            x86_64::VirtAddr::new(ptr),
            x86_64::VirtAddr::new(end),
        ) {
            return None;
        }
    }

    // Ensure all lazy pages in the range are present before the kernel reads/writes them.
    if !crate::memory::demand::prefault_user_range(&task, ptr, end) {
        return None;
    }

    // SAFETY: Transmute the guard lifetime from the stack borrow to 'static.
    // Sound because UserPtrGuard drops _guard before _task (declaration order),
    // and the Arc keeps the Task (and its vma_lock) alive.
    let guard: spin::RwLockReadGuard<'static, ()> = unsafe { core::mem::transmute(guard) };

    Some(UserPtrGuard { _guard: guard, _task: task })
}

/// Check that a user pointer is valid without returning a guard.
///
/// Use this only for early-fail checks in blocking syscalls that will
/// re-validate (with a guard) before actually dereferencing the pointer.
fn check_user_ptr(ptr: u64, size: u64) -> bool {
    validate_user_ptr(ptr, size).is_some()
}

/// Returns the current task Arc and the local CPU's kernel_id, if a task is running.
fn current_task_and_cpu() -> Option<(Arc<Task>, u32)> {
    let cpu = get_local();
    let rq = cpu.run_queue.get().unwrap().lock();
    rq.current_task.as_ref().map(|t| (t.clone(), cpu.kernel_id))
}

/// Wake a sleeping task and add it to its CPU's run queue; sends a reschedule IPI if cross-CPU.
fn wake_task(task: Arc<Task>, target_cpu_id: u32) {
    task.state.store(TaskState::Ready, Ordering::Release);
    local_scheduler::add(get_cpu(target_cpu_id), task);
    let local_id = get_local().kernel_id;
    if target_cpu_id != local_id {
        let apic_id = local_apic_id_of(target_cpu_id);
        crate::apic::send_fixed_ipi(apic_id, u8::from(crate::interrupt::InterruptVector::Reschedule));
    }
}
