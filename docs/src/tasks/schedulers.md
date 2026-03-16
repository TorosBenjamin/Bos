# Schedulers

## Local scheduler

Each CPU has its own `RunQueue` containing:

- `current_task: Option<Arc<Task>>` — the currently running task
- `ready: [VecDeque<Arc<Task>>; 3]` — three priority bands (Background, Normal, High)
- `skip_counts: [u32; 3]` — tracks how many ticks each band has been skipped
- `deferred_drop: Option<Arc<Task>>` — holds a reference to the previous task for one tick

### Scheduling algorithm

`schedule_from_interrupt()` is called from the timer handler with the current RSP:

1. **Fast path**: if `ready_count` (atomic hint) is 0, return the current context immediately. This avoids locking the run queue when there's nothing else to run.

2. **Re-queue or defer**: if the current task is still `Running`, push it back onto its priority band. If it's `Zombie` or `Sleeping`, store it in `deferred_drop` instead (explained below).

3. **Pick next task**: try High first, then Normal, then Background. The first non-empty band wins.

4. **Starvation prevention**: each time a band is skipped (a higher band had a ready task), its `skip_count` increments. When the count exceeds a threshold, the band is force-checked first:
   - Background threshold: 50 ticks (~50 ms)
   - Normal threshold: 200 ticks (~200 ms)
   - High: no threshold (never starved, always checked first)

   This ensures a Background task gets at least one tick every 50 ms even under heavy High/Normal load.

5. **Context switch**: if the next task has a different CR3 (different address space), write the new CR3 to the register. Update TSS.RSP0 to the next task's kernel stack top. Return a pointer to the next task's `CpuContext`.

### Why deferred drop

When a task exits (zombie) or sleeps, the scheduler can't drop the `Arc<Task>` immediately because:

- The current stack pointer is still pointing into the task's kernel stack (or context). Dropping the task would free the stack while the scheduler is still using it.
- Dropping an `Arc` might trigger `GuardedStack::drop()`, which takes the physical memory lock. Taking locks in the scheduler (called from an interrupt handler) risks deadlock.

Instead, the Arc is held in `deferred_drop` for one tick. On the next timer interrupt, the *next* task's stack is active, so the previous `deferred_drop` is safely overwritten (and the old Arc dropped).

### ready_count optimization

`ready_count` is an `AtomicU32` incremented when a task is added to the run queue and decremented when a task is picked. The scheduler checks it without locking the run queue — if it's zero, there's nothing to schedule and the current task continues. This avoids the overhead of locking a `Mutex` on every timer tick when only one task is running (common on APs with just the idle task).

The count is a hint, not exact — it can be briefly stale if another CPU adds a task between the check and the lock. This is fine: the task will be picked up on the next tick.

## Global scheduler

The `GlobalScheduler` manages the global task table (`BTreeMap<TaskId, Arc<Task>>`) and dispatches new tasks to CPUs.

### Task dispatch

When a new task is spawned:

1. It's inserted into the global task table (for `waitpid`, `kill`, and other lookups).
2. The least-loaded CPU is found by scanning `ready_count` across all CPUs in `Ready` state.
3. The task is added to that CPU's run queue.
4. If the chosen CPU is different from the current one, a reschedule IPI is sent to wake it from `hlt`.

### Pre-registration

`preregister_task()` inserts a task in `Loading` state into the global table before the ELF is fully parsed. This lets `sys_spawn` return the child's task ID to the parent immediately, while the child's address space is still being set up. Once loading completes, `spawn_task_activate()` transitions it to `Ready` and dispatches it.

### Zombie reaping

Kernel task zombies are reaped by the idle task (which calls `reap_zombie_kernel_tasks()` in its loop). This function:

1. Collects all zombie kernel tasks from the global table under the lock.
2. Removes them from the table.
3. Drops the collected Arcs *outside* the lock — this is important because `Drop` on `GuardedStack` takes the physical memory lock. Holding both locks simultaneously could deadlock if another path acquires them in the opposite order.

User task zombies are cleaned up differently: `sys_waitpid` removes them from the table when the parent collects the exit code.
