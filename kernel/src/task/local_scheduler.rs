use crate::memory::cpu_local_data::{CpuLocalData, get_local};
use crate::memory::MEMORY;
use crate::task::task::{Task, TaskState};
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::sync::atomic::Ordering;
use x86_64::instructions::interrupts;
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::{PhysFrame, Size4KiB};
use x86_64::PhysAddr;

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

        // If the previous task is a zombie, don't re-queue it — just drop the Arc.
        if prev_task.state.load(Ordering::Relaxed) != TaskState::Zombie {
            prev_task.state.store(TaskState::Ready, Ordering::Relaxed);
            rq.ready.push_back(prev_task);
        }
    }

    next_task.state.store(TaskState::Running, Ordering::Relaxed);
    let next_inner = next_task.inner.lock();
    let next_rsp = next_inner.rsp;
    let next_kernel_stack_top = next_inner.kernel_stack_top;
    drop(next_inner);

    // Switch address space if needed
    let next_cr3 = next_task.cr3;
    let (current_cr3_frame, _) = Cr3::read();
    let current_cr3 = current_cr3_frame.start_address().as_u64();
    if next_cr3 != current_cr3 {
        let next_frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(next_cr3));
        let cr3_flags = MEMORY.get().unwrap().new_kernel_cr3_flags;
        unsafe { Cr3::write(next_frame, cr3_flags) };
    }

    // Update TSS.RSP0 so interrupts from ring 3 land on this task's kernel stack
    unsafe { cpu.set_tss_rsp0(next_kernel_stack_top) };

    rq.current_task = Some(next_task);

    next_rsp
}
