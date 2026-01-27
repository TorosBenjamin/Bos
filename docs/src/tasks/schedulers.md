# Schedulers

## Local Scheduler

Each CPU runs its own `LocalScheduler`. It is responsible for:
- Picking the next task to run from its local run queue.
- Performing the actual context switch.
- Handling timer ticks to potentially preempt the current task.

## Global Scheduler

The `GlobalScheduler` (managed in `kernel/src/task/global_scheduler.rs`) handles:
- Spawning new tasks.
- Load balancing tasks between different CPUs.
