/// Timer-based sleep queue.
///
/// Tasks sleeping via `sys_sleep_ms` are stored here and woken by
/// `tick()`, which is called once per millisecond from `on_timer_tick`.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;
use spin::Mutex;
use crate::task::task::{Task, TaskState};

struct SleepEntry {
    wake_tsc: u64,
    task: Arc<Task>,
    cpu_id: u32,
}

static SLEEP_QUEUE: Mutex<Vec<SleepEntry>> = Mutex::new(Vec::new());

/// Enqueue a task to be woken when `wake_tsc` is reached.
/// Called from `sys_sleep_ms` with interrupts disabled (SYSCALL SFMask).
pub fn enqueue(task: Arc<Task>, cpu_id: u32, wake_tsc: u64) {
    SLEEP_QUEUE.lock().push(SleepEntry { wake_tsc, task, cpu_id });
}

/// Wake all tasks whose deadline has passed. Called from `on_timer_tick`
/// inside the timer interrupt handler (interrupts disabled, IF=0).
pub fn tick(now_tsc: u64) {
    loop {
        // Pull one expired entry without holding the lock during the wake.
        let entry = {
            let mut q = SLEEP_QUEUE.lock();
            let pos = q.iter().position(|e| e.wake_tsc <= now_tsc);
            pos.map(|i| q.swap_remove(i))
        };
        let SleepEntry { task, cpu_id, .. } = match entry {
            Some(e) => e,
            None => break,
        };
        task.state.store(TaskState::Ready, Ordering::Release);
        crate::task::local_scheduler::add(
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
