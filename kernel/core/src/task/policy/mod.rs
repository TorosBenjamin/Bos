mod round_robin;
mod priority;
mod ipc_aware;

pub use round_robin::RoundRobinPolicy;
pub use priority::PriorityPolicy;
pub use ipc_aware::IpcAwarePolicy;

use alloc::sync::Arc;
use crate::task::task::Task;

/// Trait for pluggable scheduling policies.
///
/// The scheduling *mechanism* (CR3 switch, TSS update, context save/restore,
/// deferred drop) lives in `schedule_from_interrupt` and is policy-agnostic.
/// Implementations of this trait control only which task runs next.
pub trait SchedulingPolicy {
    /// Create a new instance (called once per CPU during `init_run_queue`).
    fn new() -> Self where Self: Sized;

    /// Enqueue a task that is ready to run.
    fn enqueue(&mut self, task: Arc<Task>);

    /// Enqueue a task at the front of the queue (used for IPC-woken tasks).
    /// Default implementation falls back to `enqueue` (back of queue).
    fn enqueue_front(&mut self, task: Arc<Task>) { self.enqueue(task); }

    /// Pick the next task to run. Returns `None` if no tasks are queued.
    fn pick_next(&mut self) -> Option<Arc<Task>>;

    /// Number of tasks currently queued.
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool { self.len() == 0 }
}

/// Starvation threshold per band: ticks skipped before a forced boost.
/// Background=50ms, Normal=200ms, High=never.
///
/// Shared by `PriorityPolicy` and `IpcAwarePolicy`.
pub(crate) const STARVATION: [u32; 3] = [50, 200, u32::MAX];

/// Per-task IPC front-of-queue boost budget.
///
/// Each task starts with this many front-of-queue insertions before falling
/// back to normal (back-of-queue) enqueue. The budget resets on every normal
/// enqueue (timer preemption re-queue). This bounds starvation: a tight IPC
/// loop can only monopolise the CPU for `IPC_BOOST_BUDGET` consecutive hops
/// before other ready tasks get a turn.
pub const IPC_BOOST_BUDGET: u8 = 4;
