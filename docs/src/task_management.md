# Task Management

Bos supports preemptive multitasking through a task and scheduler system. Tasks can run in either kernel mode (ring 0) or user mode (ring 3).

## Task

A `Task` represents a unit of execution. It contains:
- A unique Task ID.
- A `TaskKind` indicating whether the task is a `Kernel` or `User` task.
- The CR3 value (physical address of its L4 page table).
- A `TaskInner` (behind a mutex) containing:
  - The saved stack pointer (`rsp`).
  - A kernel stack (`GuardedStack`) used by all tasks for interrupt and syscall handling.
  - The kernel stack top address (used to update TSS.RSP0 on context switch).
  - For user tasks: ownership of the `ManagedL4PageTable` that keeps the user address space alive.

### Task States

Tasks transition through these states:
- `Initializing` -> `Ready` -> `Running` -> `Ready` (preempted) or `Zombie` (exited).

### Kernel Tasks

Created with `Task::new(entry)`. The initial stack frame uses kernel CS/SS selectors and a trampoline function that calls the entry point. Kernel tasks share the kernel address space (same CR3).

### User Tasks

Created with `Task::new_user(entry_rip, user_rsp, page_table, cr3, user_cs, user_ss)`. The initial stack frame is placed on the task's kernel stack with user CS/SS in the iretq frame. When the scheduler first switches to this task, iretq transitions the CPU to ring 3 at the user entry point with the user stack.

Each user task has:
- Its own address space (private L4 page table with kernel higher-half cloned).
- A user stack mapped in the lower half of virtual memory.
- A kernel stack for handling interrupts and syscalls while the task is running.

## Scheduling

The kernel uses a two-level scheduling approach:
- **Global Scheduler**: Manages tasks across all CPUs.
- **Local Scheduler**: Each CPU has a local scheduler that manages the tasks currently assigned to it.

Context switching is driven by the LAPIC timer interrupt.

## Zombie Cleanup

When a task calls `sys_exit`, it is marked as a `Zombie`. The scheduler detects zombie tasks and drops them from the run queue instead of re-queuing them. The kernel stack and page table are freed when the last `Arc<Task>` reference is dropped.
