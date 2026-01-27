use crate::memory::cpu_local_data::{CpuLocalData, get_local};
use crate::task::global_scheduler::TASK_TABLE;
use crate::task::task::{TaskId, TaskState};
use alloc::collections::VecDeque;
use core::sync::atomic::Ordering;
use crate::task::{task};
use x86_64::instructions::interrupts;

pub struct RunQueue {
    pub current_task_id: Option<TaskId>,
    pub ready: VecDeque<TaskId>,
}

pub fn schedule(cpu: &CpuLocalData) {
    let _ = cpu;
    unreachable!("schedule is not safe from interrupt context; use schedule_from_interrupt");
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

pub fn get_run_queue() -> &'static spin::Mutex<RunQueue> {
    get_local().run_queue.get().expect("Run queue not initialized")
}

/// Add process to the local run queue for schedueling
pub fn add(cpu: &CpuLocalData, task_id: TaskId) {
    interrupts::without_interrupts(|| {
        let mut rq = cpu.run_queue.get().unwrap().lock();
        let tasks = TASK_TABLE.lock();

        if let Some(_) = tasks.get(&task_id) {
            rq.ready.push_back(task_id);
        } else {
            panic!("Task ID {:?} not found in TASK_TABLE", task_id);
        }
    });
}

/// Interrupt-safe scheduling: returns the next task stack pointer to load.
pub fn schedule_from_interrupt(cpu: &CpuLocalData, current_rsp: usize) -> usize {
    let mut rq = cpu.run_queue.get().unwrap().lock();

    let next_id = match rq.ready.pop_front() {
        Some(id) => id,
        None => return current_rsp, // nothing to run
    };

    let prev_id = rq.current_task_id;
    let tasks = TASK_TABLE.lock();
    let next_task = tasks.get(&next_id).expect("task disappeared").clone();

    // If there was a previous task, save its state and push it back to the ready queue
    if let Some(id) = prev_id {
        if let Some(prev_task) = tasks.get(&id) {
            prev_task.state.store(TaskState::Ready, Ordering::Relaxed);
            rq.ready.push_back(id);
            prev_task.inner.lock().rsp = current_rsp;
        }
    }

    rq.current_task_id = Some(next_id);
    next_task.state.store(TaskState::Running, Ordering::Relaxed);

    next_task.inner.lock().rsp
}
