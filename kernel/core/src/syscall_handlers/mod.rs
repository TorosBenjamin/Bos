mod task;
mod memory;
mod ipc;
mod graphics;
mod misc;
mod service;

pub use task::{sys_exit, sys_yield, sys_spawn, sys_waitpid};
pub use memory::{sys_mmap, sys_munmap, sys_create_shared_buf, sys_map_shared_buf, sys_destroy_shared_buf};
pub use ipc::{sys_channel_create, sys_channel_send, sys_channel_recv, sys_channel_close};
pub use graphics::{sys_get_bounding_box, sys_get_display_info, sys_transfer_display};
pub use misc::{sys_debug_log, sys_read_key, sys_read_mouse, sys_get_module, sys_shutdown};
pub use service::{sys_register_service, sys_lookup_service};

use alloc::sync::Arc;
use crate::memory::cpu_local_data::{get_cpu, get_local, local_apic_id_of};
use crate::task::local_scheduler;
use crate::task::task::{Task, TaskKind, TaskState};
use core::sync::atomic::Ordering;

/// Returns true if [ptr, ptr+size) is fully within the current user task's
/// allocated virtual address space and within canonical lower-half bounds.
fn validate_user_ptr(ptr: u64, size: u64) -> bool {
    if ptr == 0 || size == 0 {
        return false;
    }
    let end = match ptr.checked_add(size) {
        Some(e) => e,
        None => return false,
    };
    if ptr < crate::consts::USER_MIN || end > crate::consts::USER_MAX + 1 {
        return false;
    }
    let cpu = get_local();
    let rq = cpu.run_queue.get().unwrap().lock();
    let task = match &rq.current_task {
        Some(t) if t.kind == TaskKind::User => t.clone(),
        _ => return false,
    };
    drop(rq);
    let inner = task.inner.lock();
    crate::memory::user_vaddr::is_user_vaddr_valid_range(
        &inner.user_vaddr_set,
        x86_64::VirtAddr::new(ptr),
        x86_64::VirtAddr::new(end),
    )
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
