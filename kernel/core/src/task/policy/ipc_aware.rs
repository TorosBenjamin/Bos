use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::sync::atomic::Ordering;
use kernel_api_types::Priority;
use crate::task::task::Task;
use super::{SchedulingPolicy, STARVATION, IPC_BOOST_BUDGET};

/// Priority scheduling with IPC-aware front-of-queue insertion.
///
/// Same priority bands as `PriorityPolicy`, but tasks enqueued via
/// `enqueue_front` (IPC wakeups) go to the front of their priority band,
/// minimising IPC chain latency.
///
/// Starvation prevention uses a **per-task boost budget** (`Task::ipc_boost_remaining`).
/// Each front-of-queue insertion decrements the budget; when it reaches zero the
/// task falls back to normal back-of-queue insertion (which also resets the budget).
/// This bounds how long a tight IPC loop can monopolise the CPU before other
/// ready tasks get a turn.
pub struct IpcAwarePolicy {
    ready: [VecDeque<Arc<Task>>; 3],
    skip_counts: [u32; 3],
}

impl SchedulingPolicy for IpcAwarePolicy {
    fn new() -> Self {
        IpcAwarePolicy {
            ready: [VecDeque::new(), VecDeque::new(), VecDeque::new()],
            skip_counts: [0u32; 3],
        }
    }

    fn enqueue(&mut self, task: Arc<Task>) {
        // Normal enqueue resets the boost budget so the task gets fresh
        // front-of-queue allowance after its next IPC wakeup cycle.
        task.ipc_boost_remaining.store(IPC_BOOST_BUDGET, Ordering::Relaxed);
        let prio = Priority::from_u8(task.priority.load(Ordering::Relaxed)) as usize;
        self.ready[prio].push_back(task);
    }

    fn enqueue_front(&mut self, task: Arc<Task>) {
        let remaining = task.ipc_boost_remaining.load(Ordering::Relaxed);
        if remaining > 0 {
            task.ipc_boost_remaining.store(remaining - 1, Ordering::Relaxed);
            let prio = Priority::from_u8(task.priority.load(Ordering::Relaxed)) as usize;
            self.ready[prio].push_front(task);
        } else {
            // Budget exhausted — fall back to normal enqueue (resets budget)
            self.enqueue(task);
        }
    }

    fn pick_next(&mut self) -> Option<Arc<Task>> {
        // Starvation boost: same as PriorityPolicy
        for i in [0usize, 1] {
            if self.skip_counts[i] >= STARVATION[i] && !self.ready[i].is_empty() {
                self.skip_counts[i] = 0;
                return self.ready[i].pop_front();
            }
        }

        // Normal path: High → Normal → Background
        for winner in [2usize, 1, 0] {
            if !self.ready[winner].is_empty() {
                for lower in 0..winner {
                    if !self.ready[lower].is_empty() {
                        self.skip_counts[lower] = self.skip_counts[lower].saturating_add(1);
                    }
                }
                self.skip_counts[winner] = 0;
                return self.ready[winner].pop_front();
            }
        }

        None
    }

    fn len(&self) -> usize {
        self.ready[0].len() + self.ready[1].len() + self.ready[2].len()
    }
}
