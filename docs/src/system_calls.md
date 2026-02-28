# System Calls

System calls are the primary mechanism for userland applications to request services from the kernel.

## Implementation

- **Raw Syscall Handler**: Located in `kernel/src/raw_syscall_handler.rs`, this handles the low-level `syscall` instruction entry.
- **Syscall Handlers**: High-level handlers for specific system calls are implemented in `kernel/src/syscall_handlers.rs`.

The kernel uses the `syscall` / `sysret` instructions on x86_64 for fast system calls.

## MSR Configuration

The syscall mechanism requires three MSRs to be configured per-CPU during initialization:

- **LSTAR**: Points to `raw_syscall_handler`, the entry point for all syscalls.
- **STAR**: Encodes segment selectors. `syscall_base=0x08` gives kernel CS=0x08, SS=0x10. `sysret_base=0x10` gives user SS=0x1B, CS=0x23 (with RPL=3).
- **SFMASK**: Masks `RFLAGS.IF` during SYSCALL, disabling interrupts for the entire syscall handler. This prevents the timer from firing on the per-CPU syscall stack (which has no iretq frame and would corrupt state).

## Syscall Entry Flow

1. User executes `syscall`. CPU loads RIP from LSTAR, masks RFLAGS via SFMASK.
2. `raw_syscall_handler` saves the user RSP to a per-CPU scratch area and switches to the per-CPU syscall handler stack.
3. User's RCX (return RIP), R11 (saved RFLAGS), RSP, and the syscall number (RAX) are pushed.
4. The syscall number is dispatched via `SYS_CALL_TABLE`.
5. On return, the user's registers are restored and `sysretq` returns to ring 3.

## Diverging Syscalls

The `Exit` syscall is handled specially: since `sys_exit()` never returns, it is detected before the normal dispatch path and called directly, bypassing the sysretq return.

## Available Syscalls

| Number | Name | Status | Description |
|--------|------|--------|-------------|
| 0 | `GetBoundingBox` | Implemented | Returns the framebuffer bounding box |
| 3 | `Exit` | Implemented | Terminates the current task (marks it as zombie) |
| 4 | `Spawn` | Implemented | Spawns a new user task from ELF bytes in caller's memory |
| 5 | `ReadKey` | Implemented | Reads a keyboard event (blocking) |
| 6 | `Yield` | Implemented | Yields the current timeslice |
| 7 | `Mmap` | Implemented | Allocates virtual memory for the calling user task |
| 8 | `Munmap` | Implemented | Unmaps virtual memory from the calling user task |
| 9 | `ChannelCreate` | Implemented | Creates an IPC channel, returns send and recv endpoint IDs |
| 10 | `ChannelSend` | Implemented | Sends a message on a channel endpoint (blocks if full) |
| 11 | `ChannelRecv` | Implemented | Receives a message from a channel endpoint (blocks if empty) |
| 12 | `ChannelClose` | Implemented | Closes a channel endpoint |
| 13 | `TransferDisplay` | Implemented | Transfers display ownership to another task |
| 14 | `GetModule` | Implemented | Loads a Limine boot module by name |
| 15 | `PresentDisplay` | Implemented | Copies a dirty rectangle from user buffer to framebuffer |
| 16 | `GetDisplayInfo` | Implemented | Returns display dimensions and pixel format |

## Display Ownership

Graphics syscalls (`GetBoundingBox`, `PresentDisplay`) are restricted to the current display owner task. The kernel tracks the owner via a global `AtomicU64` storing the owner's task ID. The init task (first user task spawned by the kernel) is set as the initial display owner.

`GetDisplayInfo` does NOT require display ownership — any task can query display dimensions and pixel format.

Non-owner callers of restricted syscalls receive `GraphicsResult::PermissionDenied` (value 3).

The kernel's panic handler draws directly via the `DISPLAY` object (not via syscalls), so it bypasses this restriction.

### `PresentDisplay` (15)

**Arguments:** `buf_ptr` (rsi), `buf_width` (rdx), `dirty_x` (r10), `dirty_y` (r8), `dirty_w` (r9), `dirty_h` (rax)

Copies a dirty rectangle from a user-space u32 pixel buffer into the kernel framebuffer. The user buffer uses the same pixel encoding as the framebuffer (query via `GetDisplayInfo`). The kernel copies row-by-row from the user buffer, clamping to framebuffer bounds.

**Returns:** `GraphicsResult` code (0 = Ok, 3 = PermissionDenied).

### `GetDisplayInfo` (16)

**Arguments:** `info_out_ptr` (rsi)

Writes a `DisplayInfo` struct to the given pointer, containing the framebuffer width, height, and RGB mask information. Does not require display ownership.

**Returns:** `GraphicsResult` code.

### `TransferDisplay` (13)

**Arguments:** `new_owner_task_id` (rdi)

Transfers display ownership from the caller to the specified task. The caller must be the current display owner, and the target task must exist in the global task table.

**Returns:**
- `0` — success
- `1` — caller is not the current display owner
- `2` — target task ID not found

### `GetModule` (14)

**Arguments:** `name_ptr` (rsi), `name_len` (rdx), `buf_ptr` (r10), `buf_cap` (r8)

Loads a Limine boot module by name. Kernel prepends "/" internally to match
Limine paths (name "display_server" matches path "/display_server").

**Size query:** `buf_ptr=0, buf_cap=0` — returns module size, or 0 if not found.
**Copy:** copies module bytes to buf — returns bytes written, or 0 on failure.

## IPC Channels

Unidirectional message-passing channels for inter-task communication. Each channel has a send endpoint and a recv endpoint, identified by globally unique `u64` IDs.

### `ChannelCreate` (9)

**Arguments:** `send_ep_out_ptr` (rdi), `recv_ep_out_ptr` (rsi), `capacity` (rdx)

Creates a new channel. Writes the send endpoint ID to `*send_ep_out_ptr` and the recv endpoint ID to `*recv_ep_out_ptr`. Capacity is clamped to [1, 256]; 0 uses the default of 16.

**Returns:** IPC status code.

### `ChannelSend` (10)

**Arguments:** `endpoint_id` (rdi), `msg_ptr` (rsi), `msg_len` (rdx)

Sends `msg_len` bytes from `msg_ptr` on the given send endpoint. Maximum message size is 4 KiB. Blocks (spin-yield) if the channel is full.

**Returns:** IPC status code.

### `ChannelRecv` (11)

**Arguments:** `endpoint_id` (rdi), `buf_ptr` (rsi), `buf_cap` (rdx), `bytes_read_out_ptr` (r10)

Receives a message into the buffer at `buf_ptr` (capacity `buf_cap`). The actual number of bytes received is written to `*bytes_read_out_ptr`. Blocks (spin-yield) if the channel is empty.

**Returns:** IPC status code.

### `ChannelClose` (12)

**Arguments:** `endpoint_id` (rdi)

Closes the given endpoint. If the peer endpoint is still open, it will observe `IPC_ERR_PEER_CLOSED` on its next operation.

**Returns:** IPC status code.

### IPC Error Codes

| Constant | Value | Meaning |
|----------|-------|---------|
| `IPC_OK` | 0 | Success |
| `IPC_ERR_INVALID_ENDPOINT` | 1 | Endpoint ID not found |
| `IPC_ERR_WRONG_DIRECTION` | 2 | Send on recv endpoint or vice versa |
| `IPC_ERR_PEER_CLOSED` | 3 | Other end was closed |
| `IPC_ERR_CHANNEL_FULL` | 4 | Queue at capacity |
| `IPC_ERR_INVALID_ARGS` | 5 | Null pointer or bad parameters |
| `IPC_ERR_MSG_TOO_LARGE` | 6 | Message exceeds 4 KiB |
