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

| Number | Name | Description |
|--------|------|-------------|
| 0 | `GetBoundingBox` | Returns the framebuffer bounding box |
| 1 | `DrawIter` | Draws multiple pixels from a user-space buffer |
| 2 | `FillSolid` | Fills a rectangle with a solid color |
| 3 | `Exit` | Terminates the current task (marks it as zombie) |
| 4 | `Spawn` | Reserved for spawning new user tasks |
