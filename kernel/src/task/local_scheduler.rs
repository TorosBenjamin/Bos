use crate::memory::cpu_local_data::{CpuLocalData, get_local};
use crate::task::task::{Task, TaskState};
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::sync::atomic::Ordering;
use x86_64::instructions::interrupts;

pub struct RunQueue {
    pub current_task: Option<Arc<Task>>,
    pub ready: VecDeque<Arc<Task>>,
}

/// Safety: cpu_init must be called before
pub fn init_run_queue() {
    let cpu = get_local();

    cpu.run_queue.call_once(|| {
        spin::Mutex::new(RunQueue {
            current_task: None,
            ready: VecDeque::new(),
        })
    });
}

/// Add a task to the local run queue for scheduling.
pub fn add(cpu: &CpuLocalData, task: Arc<Task>) {
    interrupts::without_interrupts(|| {
        let mut rq = cpu.run_queue.get().unwrap().lock();
        rq.ready.push_back(task);
    });
}

/// Interrupt-safe scheduling: returns the next task stack pointer to load.
///
/// This function only locks the per-CPU run queue — it never touches TASK_TABLE,
/// so it cannot deadlock with code that holds TASK_TABLE when interrupted.
pub fn schedule_from_interrupt(cpu: &CpuLocalData, current_rsp: usize) -> usize {
    let mut rq = cpu.run_queue.get().unwrap().lock();

    let next_task = match rq.ready.pop_front() {
        Some(task) => task,
        None => return current_rsp, // nothing to switch to
    };

    // Save the current task's RSP and push it back to the ready queue
    if let Some(prev_task) = rq.current_task.take() {
        prev_task.inner.lock().rsp = current_rsp;
        prev_task.state.store(TaskState::Ready, Ordering::Relaxed);
        rq.ready.push_back(prev_task);
    }

    next_task.state.store(TaskState::Running, Ordering::Relaxed);
    let next_rsp = next_task.inner.lock().rsp;
    rq.current_task = Some(next_task);

    next_rsp
}
