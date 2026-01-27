# Task Management

Bos supports multitasking through a task and scheduler system.

## Task

A `Task` represents a unit of execution. It contains:
- A unique Task ID.
- The saved CPU context (registers, stack pointer).
- Its own stack.

## Scheduling

The kernel uses a two-level scheduling approach:
- **Global Scheduler**: Manages tasks across all CPUs.
- **Local Scheduler**: Each CPU has a local scheduler that manages the tasks currently assigned to it.

Context switching occurs either cooperatively or during the timer interrupt.
