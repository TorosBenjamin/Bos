use alloc::collections::VecDeque;
use alloc::sync::Arc;
use crate::task::task::Task;
use super::SchedulingPolicy;

/// Simple FIFO round-robin scheduling. Ignores task priority entirely.
pub struct RoundRobinPolicy {
    ready: VecDeque<Arc<Task>>,
}

impl SchedulingPolicy for RoundRobinPolicy {
    fn new() -> Self {
        RoundRobinPolicy { ready: VecDeque::new() }
    }

    fn enqueue(&mut self, task: Arc<Task>) {
        self.ready.push_back(task);
    }

    fn pick_next(&mut self) -> Option<Arc<Task>> {
        self.ready.pop_front()
    }

    fn len(&self) -> usize {
        self.ready.len()
    }
}
