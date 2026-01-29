use crate::memory::cpu_local_data::get_local;
use crate::task::task::{Task, TaskId, TaskState};
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use core::sync::atomic::Ordering;
use spin::Mutex;
use x86_64::instructions::interrupts;

pub static TASK_TABLE: Mutex<BTreeMap<TaskId, Arc<Task>>> = Mutex::new(BTreeMap::new());

pub fn spawn_task(task: Task) {
    interrupts::without_interrupts(|| {
        let task_id = task.id;
        task.state.store(TaskState::Ready, Ordering::Relaxed);
        let arc_task = Arc::new(task);

        // Insert into the global TASK_TABLE (for future lookups/kill/waitpid)
        let mut tasks = TASK_TABLE.lock();
        if tasks.insert(task_id, arc_task.clone()).is_some() {
            panic!("Task with the same ID already exists");
        }
        drop(tasks);

        // Push Arc<Task> clone to the local run queue
        let cpu = get_local();
        let mut rq = cpu.run_queue.get().unwrap().lock();
        rq.ready.push_back(arc_task);

        log::info!(
            "Task {:?} scheduled on CPU {} and pushed to ready queue",
            task_id,
            cpu.kernel_id
        );
    });
}
