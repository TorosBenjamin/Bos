use crate::interrupt::InterruptVector;
use crate::memory::cpu_local_data::{cpus_count, get_local, local_apic_id_of, try_get_ready_cpu};
use crate::task::task::{Task, TaskId, TaskState};
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;
use x86_64::instructions::interrupts;

pub static TASK_TABLE: Mutex<BTreeMap<TaskId, Arc<Task>>> = Mutex::new(BTreeMap::new());

static NEXT_SPAWN_CPU: AtomicUsize = AtomicUsize::new(0);

pub fn spawn_task(task: Task) {
    let task_id = task.id;
    let target_id = interrupts::without_interrupts(|| {
        task.state.store(TaskState::Ready, Ordering::Relaxed);
        let arc_task = Arc::new(task);

        // Insert into the global TASK_TABLE (for future lookups/kill/waitpid)
        let mut tasks = TASK_TABLE.lock();
        if tasks.insert(task_id, arc_task.clone()).is_some() {
            panic!("Task with the same ID already exists");
        }
        drop(tasks);

        // Round-robin dispatch: pick a CPU that is fully initialized (has a run queue).
        // During early boot, only the BSP is ready; APs join as they initialize.
        let total = cpus_count();
        let start = NEXT_SPAWN_CPU.fetch_add(1, Ordering::Relaxed);
        let (target_id, target_cpu) = (0..total)
            .find_map(|i| {
                let id = (start + i) % total;
                try_get_ready_cpu(id as u32).map(|cpu| (id, cpu))
            })
            .unwrap_or_else(|| {
                let local = get_local();
                (local.kernel_id as usize, local)
            });
        crate::task::local_scheduler::add(target_cpu, arc_task);

        // If target is a different CPU, send reschedule IPI to wake it from hlt
        let local = get_local();
        if target_id as u32 != local.kernel_id {
            let apic_id = local_apic_id_of(target_id as u32);
            crate::apic::send_fixed_ipi(apic_id, u8::from(InterruptVector::Reschedule));
        }

        target_id
    });

    log::info!(
        "Task {:?} scheduled on CPU {} and pushed to ready queue",
        task_id,
        target_id
    );
}
