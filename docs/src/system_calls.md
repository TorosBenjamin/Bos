# System Calls

## Entry mechanism

Bos uses the `SYSCALL`/`SYSRET` instructions for fast ring 3 to ring 0 transitions, avoiding the overhead of interrupt-based system calls.

### MSR configuration (per-CPU)

- **LSTAR** — points to `raw_syscall_handler`, the naked assembly entry point.
- **STAR** — encodes segment selectors. `syscall_base=0x08` gives kernel CS=0x08, SS=0x10. `sysret_base=0x10` gives user SS=0x1B, CS=0x23 (with RPL=3).
- **SFMASK** — masks `RFLAGS.IF` during SYSCALL, disabling interrupts for the entire entry sequence. This is critical: without it, a timer interrupt could fire while RSP still points to the user stack or before the kernel has saved user state.

### Entry flow

1. User executes `syscall`. CPU loads RIP from LSTAR, saves user RIP to RCX, saves RFLAGS to R11, masks RFLAGS via SFMASK.
2. `raw_syscall_handler` (naked) executes `swapgs` to get kernel GS (per-CPU data).
3. Saves user RSP to a per-CPU scratch slot, loads kernel stack pointer from `current_task_kernel_stack_top`.
4. Saves all 15 GPRs and builds an iretq frame in the current task's `CpuContext`. This means the timer can preempt a sleeping syscall (see below) and context-switch to another task — the syscall's state is fully captured.
5. Sets the `in_syscall_handler` flag so the timer handler knows to skip register saving (already done).
6. Converts R10 to RCX per the x86-64 ABI (SYSCALL clobbers RCX, so the 4th argument is passed in R10 instead).
7. Calls the Rust `syscall_handler()` function.

### Return flow

After the syscall handler returns:

1. Clears `in_syscall_handler`.
2. Restores user RSP, loads return value into RAX.
3. `swapgs` back to user GS.
4. `sysretq` — sets RIP from RCX, restores RFLAGS from R11, transitions to ring 3.

### Diverging syscalls

`Exit` never returns — it marks the task as zombie and yields. It's detected before the normal dispatch path and called directly, bypassing `sysretq`.

### Preemption during blocking syscalls

When a syscall blocks (e.g., `channel_recv` on an empty channel), it:

1. Sets the task state to `Sleeping` and registers as a waiter.
2. Clears `in_syscall_handler` so the timer handler saves kernel state properly.
3. Enables interrupts and executes `hlt`.
4. When woken (waiter fires), the timer resumes the task at the `hlt` instruction, the loop re-checks the condition, and eventually returns normally via `sysretq`.

## User pointer validation

Before reading or writing user memory, the kernel must verify the pointer:

1. **Bounds check**: pointer must be in `[USER_MIN, USER_MAX]`.
2. **VMA check**: entire range must be covered by a valid VMA.
3. **Prefault**: demand-fill any lazy pages so the kernel won't page-fault mid-copy.
4. **Guard**: return a `UserPtrGuard` that holds the VMA read lock, preventing concurrent `munmap` from pulling pages out.

The guard is an RAII struct — it releases the read lock when dropped. This means the kernel holds the lock exactly as long as it's using the pointer, with no risk of forgetting to release it.

## Syscall table

| # | Name | Category | Description |
|---|------|----------|-------------|
| 0 | `GetBoundingBox` | Graphics | Returns framebuffer bounding box (owner only) |
| 3 | `Exit` | Task | Terminate current task (diverging) |
| 4 | `Spawn` | Task | Create child task from ELF bytes in caller's memory |
| 5 | `ReadKey` | Input | Read keyboard event (blocks if none) |
| 6 | `Yield` | Task | Voluntarily give up timeslice |
| 7 | `Mmap` | Memory | Allocate virtual memory (demand-paged) |
| 8 | `Munmap` | Memory | Unmap virtual memory |
| 9 | `ChannelCreate` | IPC | Create channel, get send + recv endpoint IDs |
| 10 | `ChannelSend` | IPC | Send message (blocks if full) |
| 11 | `ChannelRecv` | IPC | Receive message (blocks if empty) |
| 12 | `ChannelClose` | IPC | Close an endpoint |
| 13 | `TransferDisplay` | Graphics | Transfer display ownership to another task |
| 14 | `GetModule` | Misc | Load a Limine boot module by name |
| 15 | `GetDisplayInfo` | Graphics | Get display dimensions and pixel format |
| 16 | `DebugLog` | Misc | Write string to kernel log (serial) |
| 17 | `Waitpid` | Task | Wait for child to exit, get exit code (blocks) |
| 18 | `RegisterService` | Service | Register a named service (IPC endpoint) |
| 19 | `LookupService` | Service | Look up a service by name |
| 20 | `ReadMouse` | Input | Read mouse event (non-blocking) |
| 21 | `Shutdown` | Misc | Shut down the machine (QEMU exit) |
| 22 | `CreateSharedBuf` | Memory | Allocate shared buffer, get ID + mapped address |
| 23 | `MapSharedBuf` | Memory | Map existing shared buffer into caller's space |
| 24 | `DestroySharedBuf` | Memory | Destroy shared buffer, free frames |
| 25 | `BlockReadSectors` | Disk | Read disk sectors via DMA/PIO |
| 26 | `BlockWriteSectors` | Disk | Write disk sectors |
| 27 | `ThreadCreate` | Task | Create thread sharing parent's address space |
| 28 | `SetExitChannel` | Task | Set IPC endpoint for exit notifications |
| 29 | `TryReadKey` | Input | Non-blocking keyboard read |
| 30 | `TryChannelRecv` | IPC | Non-blocking channel receive |
| 31 | `TryChannelSend` | IPC | Non-blocking channel send |
| 32 | `WaitForEvent` | Event | Wait for multiple event sources (channel, key, mouse, timeout) |
| 33 | `SleepMs` | Task | Sleep for N milliseconds |
| 34 | `SetPriority` | Task | Change task priority (Background/Normal/High) |
| 35 | `GetTimeNs` | Time | Get wall-clock time in nanoseconds |
| 36 | `Mprotect` | Memory | Change page permissions on existing mapping |
| 37 | `Mremap` | Memory | Resize or move existing mapping |
| 38 | `SetFaultEp` | Task | Set IPC endpoint for child fault notifications |
| 39 | `WaitTaskReady` | Task | Block until a Loading task becomes Ready |
| 40 | `PciConfigRead` | PCI | Read PCI configuration space |
| 41 | `PciConfigWrite` | PCI | Write PCI configuration space |
| 42 | `MapPciBar` | PCI | Map PCI BAR into caller's address space |
| 43 | `AllocDma` | Memory | Allocate physically-contiguous DMA buffer |

## IPC channels

Channels are the primary inter-task communication mechanism. Each channel is a bounded, unidirectional message queue with two endpoints: one for sending, one for receiving.

### Design

- **Bounded queue**: capacity defaults to 16, configurable up to 256 messages. Bounded to prevent a fast sender from consuming unbounded kernel memory.
- **Max message size**: 4 KiB. Chosen to match page size — large enough for control messages and small data transfers. Bulk data should use shared buffers instead.
- **Blocking semantics**: `ChannelSend` blocks if the queue is full; `ChannelRecv` blocks if empty. Non-blocking variants (`TryChannelSend`/`TryChannelRecv`) return immediately with an error code.
- **Peer close detection**: when one endpoint is closed, the other sees `IPC_ERR_PEER_CLOSED` on its next operation. All blocked waiters are woken.

### Blocking implementation

When a sender blocks:

1. The message is copied into a kernel buffer (releasing the VMA guard so the lock isn't held across `hlt`).
2. The task state is set to `Sleeping` and the task is added to the channel's `send_waiters` queue — both under the same lock, so a concurrent receiver sees `Sleeping` when it pops the waiter.
3. A re-check occurs: if space freed between `try_send` and registration, the task is immediately re-woken (avoids missed wakeup).
4. `in_syscall_handler` is cleared, interrupts are enabled, and `hlt` is executed.
5. On wakeup, the loop retries `try_send`.

### Error codes

| Code | Value | Meaning |
|------|-------|---------|
| `IPC_OK` | 0 | Success |
| `IPC_ERR_INVALID_ENDPOINT` | 1 | Endpoint ID not found |
| `IPC_ERR_WRONG_DIRECTION` | 2 | Send on recv endpoint or vice versa |
| `IPC_ERR_PEER_CLOSED` | 3 | Other end was closed |
| `IPC_ERR_CHANNEL_FULL` | 4 | Queue at capacity |
| `IPC_ERR_INVALID_ARGS` | 5 | Null pointer or bad parameters |
| `IPC_ERR_MSG_TOO_LARGE` | 6 | Message exceeds 4 KiB |

## Shared buffers

Shared buffers provide zero-copy memory sharing between tasks. Unlike IPC messages (which are copied through the kernel), shared buffer frames are mapped into both tasks' address spaces at the same virtual addresses.

1. **Create**: allocates N physical frames (tagged `SharedBuffer`), maps them into the creator's address space, registers the buffer with a global ID.
2. **Map**: maps the same physical frames into another task's address space. Both tasks see the same memory.
3. **Destroy**: unmaps from all tasks, frees the physical frames.

The `SharedBuffer` memory type tag prevents task exit cleanup from accidentally freeing frames that other tasks are still using.

## Display ownership

Graphics syscalls (`GetBoundingBox`, `PresentDisplay`) are restricted to the current display owner. A global `AtomicU64` tracks the owner's task ID (initially the first user task). Non-owners receive `GraphicsResult::PermissionDenied`.

`TransferDisplay` atomically transfers ownership. `GetDisplayInfo` is unrestricted — any task can query display dimensions.

This model exists because the framebuffer is a single shared resource. Without ownership, two tasks writing to it simultaneously would produce visual corruption. The display server pattern (one owner, other tasks communicate via IPC) avoids this.

## Service registry

The service registry provides name-based service discovery:

- `RegisterService(name, send_endpoint)`: maps a name to an IPC send endpoint.
- `LookupService(name)`: returns the send endpoint for that name.

This decouples service consumers from producers — a task can look up "display_server" by name without knowing its task ID or endpoint ID. Services are automatically unregistered when the owning task exits.
