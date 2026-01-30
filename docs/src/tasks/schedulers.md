# Schedulers

## Local Scheduler

Each CPU runs its own local scheduler via `schedule_from_interrupt`. It is responsible for:
- Picking the next task to run from its local run queue.
- Performing the actual context switch (saving/restoring RSP).
- Switching address spaces by writing to CR3 when the next task uses a different page table.
- Updating TSS.RSP0 to point to the next task's kernel stack top, so that interrupts from ring 3 land on the correct kernel stack.
- Detecting and dropping zombie tasks instead of re-queuing them.

### Context Switch Flow

When the LAPIC timer fires:

1. The timer interrupt handler pushes all 15 GPRs onto the current stack.
2. `schedule_from_interrupt` is called with the current RSP.
3. The previous task's RSP is saved in its `TaskInner`.
4. If the previous task is a zombie, it is dropped from the queue.
5. The next task is popped from the ready queue.
6. If the next task has a different CR3, the CPU switches address spaces.
7. TSS.RSP0 is updated to the next task's kernel stack top.
8. The next task's saved RSP is returned.
9. The timer handler restores GPRs from the new stack and executes iretq.

For user tasks, iretq with user CS/SS returns the CPU to ring 3. For kernel tasks, iretq with kernel CS/SS stays in ring 0.

## Global Scheduler

The `GlobalScheduler` (managed in `kernel/src/task/global_scheduler.rs`) handles:
- Spawning new tasks and inserting them into the global task table.
- Pushing new tasks to the local CPU's run queue.
