# System Calls

System calls are the primary mechanism for userland applications to request services from the kernel.

## Implementation

- **Raw Syscall Handler**: Located in `kernel/src/raw_syscall_handler.rs`, this handles the low-level `syscall` instruction entry.
- **Syscall Handlers**: High-level handlers for specific system calls are implemented in `kernel/src/syscall_handlers.rs`.

The kernel uses the `syscall` / `sysret` instructions on x86_64 for fast system calls.
