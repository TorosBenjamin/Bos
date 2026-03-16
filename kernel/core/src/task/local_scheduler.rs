use crate::memory::cpu_local_data::{CpuLocalData, get_local, get_cpu, local_apic_id_of};
use crate::time::tsc;
use crate::memory::MEMORY;
use crate::task::task::{CpuContext, Task, TaskState};
use kernel_api_types::Priority;

/// A waiter slot used by `sys_wait_for_event` to register a sleeping task
/// against a single event source. Woken via `try_wake_slot` which uses a CAS
/// to ensure the task is woken at most once even if multiple sources fire.
pub type EventWaiterSlot = spin::Mutex<Option<(Arc<Task>, u32)>>;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::sync::atomic::Ordering;
use x86_64::instructions::interrupts;
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::{PhysFrame, Size4KiB};
use x86_64::PhysAddr;

pub struct RunQueue {
    pub current_task: Option<Arc<Task>>,
    /// Index 0 = Background, 1 = Normal, 2 = High
    pub ready: [VecDeque<Arc<Task>>; 3],
    /// How many ticks each band has been skipped without running.
    pub skip_counts: [u32; 3],
    /// Holds the Arc of a zombie/sleeping task for exactly one scheduler tick.
    ///
    /// When a Zombie/Sleeping task is dequeued from `current_task`, dropping its
    /// Arc would call `GuardedStack::drop`, unmapping the very kernel stack the
    /// scheduler is currently running on. To avoid this, we defer the drop here.
    /// On the *next* call to `schedule_from_interrupt` (when we're on a different
    /// task's stack), this slot is cleared first — safely dropping the old Arc.
    deferred_drop: Option<Arc<Task>>,
}

/// Starvation threshold per band: ticks skipped before a forced boost.
/// Background=50ms, Normal=200ms, High=never.
const STARVATION: [u32; 3] = [50, 200, u32::MAX];

/// Pick the next task to run using priority with starvation protection.
fn pick_next(rq: &mut RunQueue) -> Option<Arc<Task>> {
    // Starvation boost: lowest-priority bands first
    for i in [0usize, 1] {
        if rq.skip_counts[i] >= STARVATION[i] && !rq.ready[i].is_empty() {
            rq.skip_counts[i] = 0;
            return rq.ready[i].pop_front();
        }
    }

    // Normal path: High → Normal → Background
    for winner in [2usize, 1, 0] {
        if !rq.ready[winner].is_empty() {
            // Increment skip counts for all lower non-empty bands
            for lower in 0..winner {
                if !rq.ready[lower].is_empty() {
                    rq.skip_counts[lower] = rq.skip_counts[lower].saturating_add(1);
                }
            }
            rq.skip_counts[winner] = 0;
            return rq.ready[winner].pop_front();
        }
    }

    None
}

/// Safety: cpu_init must be called before
pub fn init_run_queue() {
    let cpu = get_local();

    cpu.run_queue.call_once(|| {
        spin::Mutex::new(RunQueue {
            current_task: None,
            ready: [VecDeque::new(), VecDeque::new(), VecDeque::new()],
            skip_counts: [0u32; 3],
            deferred_drop: None,
        })
    });
}

/// Add a task to the local run queue for scheduling.
pub fn add(cpu: &CpuLocalData, task: Arc<Task>) {
    interrupts::without_interrupts(|| {
        let mut rq = cpu.run_queue.get().unwrap().lock();
        let prio = Priority::from_u8(task.priority.load(Ordering::Relaxed)) as usize;
        rq.ready[prio].push_back(task);
        cpu.ready_count.fetch_add(1, Ordering::Relaxed);
    });
}

/// Wake the task in `slot` (if any) using a CAS to guard against double-wakeup.
///
/// Takes the Arc out of the slot atomically, then tries `compare_exchange(Sleeping, Ready)`.
/// If the task has already been woken by another source the CAS fails and the Arc is dropped.
pub fn try_wake_slot(slot: &EventWaiterSlot) {
    if let Some((task, cpu_id)) = slot.lock().take() {
        use core::sync::atomic::Ordering::{AcqRel, Relaxed};
        if task.state.compare_exchange(TaskState::Sleeping, TaskState::Ready, AcqRel, Relaxed).is_ok() {
            add(get_cpu(cpu_id), task);
            let local_id = get_local().kernel_id;
            if cpu_id != local_id {
                let apic_id = local_apic_id_of(cpu_id);
                crate::apic::send_fixed_ipi(apic_id, u8::from(crate::interrupt::InterruptVector::Reschedule));
            }
        }
    }
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
    // Fast path: nothing queued — skip lock acquisition entirely.
    // ready_count is a hint (another CPU may add a task between this check and the
    // lock), so a missed tick is fine; the task will be picked up next time.
    if cpu.ready_count.load(Ordering::Relaxed) == 0 {
        return cpu.current_context_ptr.load(Ordering::Relaxed);
    }

    let mut rq = cpu.run_queue.get().unwrap().lock();

    // Drop any task Arc deferred from the previous tick. We're now on a different
    // task's kernel stack so it's safe to run that task's GuardedStack::drop here.
    drop(rq.deferred_drop.take());

    // Get pointer to current context (saved by timer handler)
    let current_ctx_ptr = cpu.current_context_ptr.load(Ordering::Relaxed);

    let next_task = match pick_next(&mut rq) {
        Some(task) => {
            cpu.ready_count.fetch_sub(1, Ordering::Relaxed);
            task
        }
        None => {
            // ready_count was non-zero but all queues are empty — a concurrent steal
            // or pop raced with us. Nothing to switch to.
            return current_ctx_ptr;
        }
    };

    // Read TSC once: used to charge the outgoing task and stamp the incoming one.
    let now_tsc = tsc::value();

    // Re-queue the current task if it's still runnable
    if let Some(prev_task) = rq.current_task.take() {
        // Charge one quantum to the outgoing task
        prev_task.cpu_ticks.fetch_add(1, Ordering::Relaxed);

        // Accumulate fine-grained CPU time from the TSC slice.
        let ticks_per_ms = crate::time::tsc::TSC_TICKS_PER_MS.load(Ordering::Relaxed);
        if ticks_per_ms > 0 {
            let start = prev_task.slice_start_tsc.load(Ordering::Relaxed);
            if start > 0 {
                let elapsed_tsc = now_tsc.saturating_sub(start);
                // elapsed_tsc * 1_000_000 ns/ms / ticks_per_ms
                let elapsed_ns = elapsed_tsc.saturating_mul(1_000_000) / ticks_per_ms;
                prev_task.cpu_ns.fetch_add(elapsed_ns, Ordering::Relaxed);
            }
        }

        match prev_task.state.load(Ordering::Relaxed) {
            // Zombie/Sleeping: defer the Arc drop to the NEXT scheduler tick so we
            // don't trigger GuardedStack::drop while still on this task's kernel stack.
            TaskState::Zombie | TaskState::Sleeping => {
                rq.deferred_drop = Some(prev_task);
            }
            _ => {
                let prev_prio = Priority::from_u8(prev_task.priority.load(Ordering::Relaxed)) as usize;
                prev_task.state.store(TaskState::Ready, Ordering::Relaxed);
                rq.ready[prev_prio].push_back(prev_task);
                cpu.ready_count.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    next_task.state.store(TaskState::Running, Ordering::Relaxed);
    let mut next_inner = next_task.inner.lock();
    let next_kernel_stack_top = next_inner.kernel_stack_top;

    // Get pointer to next task's context
    let next_ctx_ptr = &mut next_inner.context as *mut CpuContext;

    drop(next_inner);

    // Switch address space if needed
    let next_cr3 = next_task.cr3.load(Ordering::Relaxed);
    let (current_cr3_frame, _) = Cr3::read();
    let current_cr3 = current_cr3_frame.start_address().as_u64();
    if next_cr3 != current_cr3 {
        let next_frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(next_cr3));
        let cr3_flags = MEMORY.get().unwrap().new_kernel_cr3_flags;
        unsafe { Cr3::write(next_frame, cr3_flags) };
    }

    // Update TSS.RSP0 so interrupts from ring 3 land on this task's kernel stack
    unsafe { cpu.set_tss_rsp0(next_kernel_stack_top) };

    // Verify the context is valid (debug only — panicking inside an ISR with a
    // corrupt context risks a double fault in release builds)
    #[cfg(debug_assertions)]
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

    // Stamp the incoming task's slice start so the next switch can charge it.
    next_task.slice_start_tsc.store(now_tsc, Ordering::Relaxed);

    rq.current_task = Some(next_task);

    // Update per-CPU current context pointer (timer handler will also do this,
    // but we need it updated for nested scenarios)
    cpu.current_context_ptr.store(next_ctx_ptr, Ordering::Relaxed);

    next_ctx_ptr
}
