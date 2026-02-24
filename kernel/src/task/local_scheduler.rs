use crate::memory::cpu_local_data::{CpuLocalData, get_local};
use crate::memory::MEMORY;
use crate::task::task::{CpuContext, Task, TaskState};
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

/// Interrupt-safe scheduling: returns pointer to next task's CpuContext.
///
/// The caller (timer interrupt handler) has already saved the current task's
/// context to its CpuContext struct. This function:
/// 1. Re-queues the current task if it's still runnable
/// 2. Picks the next task from the ready queue
/// 3. Switches CR3 and TSS.RSP0 as needed
/// 4. Returns pointer to next task's context (for the timer handler to restore)
///
/// This function only locks the per-CPU run queue — it never touches TASK_TABLE,
/// so it cannot deadlock with code that holds TASK_TABLE when interrupted.
pub fn schedule_from_interrupt(cpu: &CpuLocalData) -> *mut CpuContext {
    let mut rq = cpu.run_queue.get().unwrap().lock();

    // Get pointer to current context (saved by timer handler)
    let current_ctx_ptr = cpu.current_context_ptr.load(Ordering::Relaxed);

    let next_task = match rq.ready.pop_front() {
        Some(task) => task,
        None => {
            // No task to switch to - return current context
            return current_ctx_ptr;
        }
    };

    // Re-queue the current task if it's still runnable
    if let Some(prev_task) = rq.current_task.take() {
        // Context is already saved by timer handler - no need to save RSP

        match prev_task.state.load(Ordering::Relaxed) {
            // Zombie: being cleaned up by scheduler drop
            // Sleeping: waiter slot holds the only remaining Arc; just drop this one
            TaskState::Zombie | TaskState::Sleeping => {}
            _ => {
                prev_task.state.store(TaskState::Ready, Ordering::Relaxed);
                rq.ready.push_back(prev_task);
            }
        }
    }

    next_task.state.store(TaskState::Running, Ordering::Relaxed);
    let mut next_inner = next_task.inner.lock();
    let next_kernel_stack_top = next_inner.kernel_stack_top;

    // Get pointer to next task's context
    let next_ctx_ptr = &mut next_inner.context as *mut CpuContext;

    drop(next_inner);

    // Debug: check if kernel stack is in framebuffer physical range
    let hhdm = crate::memory::hhdm_offset::hhdm_offset().as_u64();
    let stack_phys = next_kernel_stack_top - hhdm;
    if stack_phys >= 0x80000000 && stack_phys < 0x80400000 {
        panic!("KERNEL STACK OVERLAPS FRAMEBUFFER! stack_top={:#x} phys={:#x}",
            next_kernel_stack_top, stack_phys);
    }

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

    // Verify the context is valid
    {
        let ctx = unsafe { &*next_ctx_ptr };
        // CS should be 0x08 (kernel) or 0x23 (user)
        if ctx.cs != 0x08 && ctx.cs != 0x23 {
            panic!(
                "SCHED: task {} has invalid context: rip={:#x} cs={:#x} fl={:#x} rsp={:#x} ss={:#x}",
                next_task.id.to_u64(), ctx.rip, ctx.cs, ctx.rflags, ctx.rsp, ctx.ss
            );
        }
    }

    rq.current_task = Some(next_task);

    // Update per-CPU current context pointer (timer handler will also do this,
    // but we need it updated for nested scenarios)
    cpu.current_context_ptr.store(next_ctx_ptr, Ordering::Relaxed);

    next_ctx_ptr
}
