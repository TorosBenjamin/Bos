# Task Management

## Task structure

A `Task` represents a unit of execution. It is reference-counted (`Arc<Task>`) because multiple places hold references simultaneously: the run queue, the global task table, IPC wait queues, and syscall handlers.

### Key fields

| Field | Type | Purpose |
|-------|------|---------|
| `id` | `TaskId` | Globally unique, monotonically allocated |
| `kind` | `TaskKind` | `Kernel` or `User` |
| `priority` | `AtomicU8` | 0=Background, 1=Normal, 2=High |
| `state` | `AtomicTaskState` | Current lifecycle state |
| `cr3` | `AtomicU64` | Physical address of L4 page table |
| `vma_lock` | `RwLock<()>` | Protects user VMAs from concurrent modification |
| `inner` | `Mutex<TaskInner>` | Mutable state (context, stacks, VMAs, endpoints) |

### CpuContext

Each task has a `CpuContext` (160 bytes) that stores all registers needed to resume execution:

```
Offset  0-119:  15 GPRs (r15, r14, ..., rax)
Offset 120-159: iretq frame (rip, cs, rflags, rsp, ss)
```

The layout matches the order that the naked timer handler pushes/pops registers. When the scheduler switches tasks, it just saves the current RSP pointing into one CpuContext and loads the RSP pointing into another — all register state is on the (conceptual) stack within the context struct.

### Task states

```
Loading -> Initializing -> Ready <-> Running
                             |          |
                             v          v
                          Sleeping    Zombie
                             |
                             v
                           Ready (woken)
```

- **Loading**: task ID is allocated and visible in the global table, but the task isn't runnable yet (ELF is still being parsed). This allows `sys_spawn` to return the child's task ID before the child is fully loaded.
- **Initializing**: task is constructed but not yet on a run queue.
- **Ready**: on a run queue, waiting to be scheduled.
- **Running**: actively executing on a CPU.
- **Sleeping**: blocked on IPC, keyboard input, or a timer. Removed from the run queue. Woken by the event source (IPC send/recv, keyboard interrupt, timer tick).
- **Zombie**: `sys_exit` called or task killed by exception. The scheduler drops it from the queue on the next tick.

State transitions use atomic CAS operations (e.g., `Sleeping -> Ready`) to prevent double-wakeups from concurrent event sources.

## Task creation

### Kernel tasks

Created with `Task::new(entry_fn)`. The initial CpuContext places the entry function's address in the RIP field with kernel CS/SS. When the scheduler first switches to this task, `iretq` jumps to the entry function in ring 0. Kernel tasks share the kernel's CR3 (no private address space).

### User tasks

Created with `Task::new_user(entry_rip, user_rsp, page_table, cr3)`. The CpuContext uses user CS (0x23) and SS (0x1B) with RPL=3. When `iretq` fires, the CPU transitions to ring 3 at the ELF entry point with the user stack.

Each user task has:
- Its own L4 page table (kernel higher-half cloned, user-space private)
- A kernel stack (for handling interrupts and syscalls while this task runs)
- VMAs tracking all mapped user memory

### Threads

`Task::new_thread(entry_rip, user_rsp, parent_task)` creates a thread that shares the parent's address space (same CR3 and page table frame). Threads have independent kernel stacks and register state but share all user memory.

## VMA lock (TOCTOU protection)

The `vma_lock` field is an `RwLock<()>` that prevents a race between pointer validation and kernel use:

- **Read lock**: held by `validate_user_ptr()` while the kernel accesses user memory. Multiple syscalls can hold the read lock concurrently.
- **Write lock**: held by `sys_munmap`, `sys_mprotect`, and `sys_mremap` before modifying VMAs or unmapping pages.

Without this lock, one thread could call `sys_munmap` while another thread's syscall handler is in the middle of copying data from user memory — the pages would vanish mid-copy, causing a kernel page fault.

Blocking syscalls (IPC recv, keyboard read) can't hold the read lock across `hlt()` because that would deadlock any thread trying to munmap. Instead, they:
- Copy input data to a kernel buffer while the lock is held (e.g., `sys_channel_send`)
- Re-validate with a fresh guard just before writing output (e.g., `sys_channel_recv`)

## Zombie cleanup

When a task calls `sys_exit`:

1. The task's state is set to `Zombie`.
2. All owned IPC endpoints are closed (waking any blocked peers).
3. All registered services are unregistered.
4. If an `exit_channel` is set, the exit code is sent to it (for supervisor notification).
5. The scheduler sees the zombie state on the next tick and drops it from the run queue.

For kernel tasks, zombie reaping happens in the idle task (not interrupt context) because dropping an `Arc<Task>` may trigger `GuardedStack::drop()`, which frees physical frames — an operation that takes the physical memory lock and shouldn't happen in an interrupt handler.
