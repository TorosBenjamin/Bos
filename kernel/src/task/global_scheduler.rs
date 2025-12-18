use crate::memory::cpu_local_data::{CpuLocalData, get_local};
use crate::task::task::{Task, TaskId, TaskState};
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use core::sync::atomic::Ordering;
use spin::Mutex;

pub static TASK_TABLE: Mutex<BTreeMap<TaskId, Arc<Task>>> = Mutex::new(BTreeMap::new());

pub fn spawn_task(task: Arc<Task>) {
    // Insert into the global TASK_TABLE
    let task_id = task.id;
    let mut tasks = TASK_TABLE.lock();
    if tasks.insert(task_id, task.clone()).is_some() {
        panic!("Task with the same ID already exists");
    }

    // For now only adds it to the callers tasks
    // TODO: Add load balancing
    let cpu = get_local();
    let mut rq = cpu.run_queue.get().unwrap().lock();
    task.state.store(TaskState::Ready, Ordering::Relaxed);
    rq.ready.push_back(task_id);
}
