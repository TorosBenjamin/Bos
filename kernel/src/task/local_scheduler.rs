use crate::memory::cpu_local_data::{CpuLocalData, get_local};
use crate::task::global_scheduler::TASK_TABLE;
use crate::task::task::{TaskId, TaskState};
use alloc::collections::VecDeque;
use core::sync::atomic::Ordering;
use crate::task::{task};

pub struct RunQueue {
    pub current_task_id: Option<TaskId>,
    pub ready: VecDeque<TaskId>,
}

pub fn schedule(cpu: &CpuLocalData) {
    let mut rq = cpu.run_queue.get().unwrap().lock();

    for task_id in rq.ready.iter() {
        log::info!("{}", task_id.to_usize());
    }

    let next_id = match rq.ready.pop_front() {
        Some(id) => id,
        None => return, // nothing to run
    };

    let tasks = TASK_TABLE.lock();

    let next_task = tasks.get(&next_id).expect("task disappeared").clone();

    // Get previous task if exists
    let prev_task_opt = rq.current_task_id.and_then(|id| tasks.get(&id).cloned());

    // Update current task
    rq.current_task_id = Some(next_id);

    // Update states
    next_task.state.store(TaskState::Running, Ordering::Relaxed);

    // If there was a previous task, mark it ready and push back
    if let Some(prev_task) = prev_task_opt {
        prev_task.state.store(TaskState::Ready, Ordering::Relaxed);
        rq.ready.push_back(prev_task.id);
        task::switch(&prev_task, &next_task);
    } else {
        task::switch_to_new(&next_task);
    }
}

/// Safety: cpu_init must be called before
pub fn init_run_queue() {
    let cpu = get_local();

    cpu.run_queue.call_once(|| {
        spin::Mutex::new(RunQueue {
            current_task_id: None,
            ready: VecDeque::new(),
        })
    });
}

/// Add process to the local run queue for schedueling
pub fn add(cpu: &CpuLocalData, task_id: TaskId) {
    let mut rq = cpu.run_queue.get().unwrap().lock();
    let tasks = TASK_TABLE.lock();

    if let Some(_) = tasks.get(&task_id) {
        rq.ready.push_back(task_id);
    } else {
        panic!("Task ID {:?} not found in TASK_TABLE", task_id);
    }
}
