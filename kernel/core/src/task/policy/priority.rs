use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::sync::atomic::Ordering;
use kernel_api_types::Priority;
use crate::task::task::Task;
use super::{SchedulingPolicy, STARVATION};

/// Priority-based scheduling with starvation protection.
///
/// Three bands (Background / Normal / High). Lower bands get a forced boost
/// after being skipped for `STARVATION[band]` ticks.
pub struct PriorityPolicy {
    /// Index 0 = Background, 1 = Normal, 2 = High
    ready: [VecDeque<Arc<Task>>; 3],
    /// How many ticks each band has been skipped without running.
    skip_counts: [u32; 3],
}

impl SchedulingPolicy for PriorityPolicy {
    fn new() -> Self {
        PriorityPolicy {
            ready: [VecDeque::new(), VecDeque::new(), VecDeque::new()],
            skip_counts: [0u32; 3],
        }
    }

    fn enqueue(&mut self, task: Arc<Task>) {
        let prio = Priority::from_u8(task.priority.load(Ordering::Relaxed)) as usize;
        self.ready[prio].push_back(task);
    }

    fn pick_next(&mut self) -> Option<Arc<Task>> {
        // Starvation boost: lowest-priority bands first
        for i in [0usize, 1] {
            if self.skip_counts[i] >= STARVATION[i] && !self.ready[i].is_empty() {
                self.skip_counts[i] = 0;
                return self.ready[i].pop_front();
            }
        }

        // Normal path: High → Normal → Background
        for winner in [2usize, 1, 0] {
            if !self.ready[winner].is_empty() {
                // Increment skip counts for all lower non-empty bands
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
