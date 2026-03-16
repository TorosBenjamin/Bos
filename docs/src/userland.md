# Userland

## User task creation from ELF

User tasks are loaded from ELF binaries (either Limine boot modules or bytes in the parent's memory via `sys_spawn`). The loading process:

1. **Validate ELF**: check magic, 64-bit, little-endian, executable. Verify all LOAD segments are in the lower half (`< 0x7FFF_FFFF_FFFF`) and don't overlap (checked at page granularity).

2. **Create address space**: allocate a new L4 frame, clone kernel higher-half entries (256-511) so kernel code and HHDM remain accessible.

3. **Map LOAD segments**: for each PT_LOAD header:
   - Translate ELF flags (R/W/X) to page table flags (PRESENT, USER_ACCESSIBLE, WRITABLE, NO_EXECUTE).
   - Allocate a VMA for the segment's virtual address range.
   - Allocate physical frames, copy segment data, zero BSS regions.
   - Install page table entries. These segments are `EagerlyMapped` — all frames are present from the start.

4. **Allocate user stack**: a 256 KiB stack VMA at the top of user space. The stack is `Anonymous` (demand-paged) — pages are allocated on first access rather than upfront, saving memory for tasks that use little stack.

5. **Create task**: build a `CpuContext` with the ELF entry point as RIP, stack top as RSP, and user CS/SS (0x23/0x1B). When the scheduler first runs this task, `iretq` transitions the CPU to ring 3.

## Address space layout

```
0x0000_0000_1000                    ELF code/data (LOAD segments)
      ...                           mmap regions (anonymous, demand-paged)
      ...                           Shared buffer mappings
0x7FFF_FFBF_F000 - 0x7FFF_FFFF_FFFF  User stack (256 KiB, grows downward)
─── canonical hole ───
0xFFFF_8000_0000_0000+              Kernel (HHDM + kernel image, shared)
```

`USER_MIN = 0x1000` (not 0x0) so null pointer dereferences fault immediately. The gap between 0x0 and 0x1000 is intentionally left unmapped.

## Interrupt handling from ring 3

When a hardware interrupt fires while a user task is running:

1. The CPU reads RSP0 from the TSS → switches to the task's kernel stack.
2. The CPU pushes the iretq frame: user SS, RSP, RFLAGS, CS, RIP.
3. The naked handler executes `swapgs` (switches GS to kernel per-CPU data).
4. The handler saves GPRs to the task's `CpuContext`.
5. The scheduler may switch to a different task.
6. When resumed, GPRs and iretq frame are restored, `swapgs` back, and `iretq` returns to ring 3.

Each user task has its own kernel stack, so interrupt state is preserved across context switches. The timer handler does NOT use IST for this reason — if it used a shared IST stack, context-switching away would lose the saved state.

## Syscall ABI

User tasks invoke syscalls using the `syscall` instruction:

| Register | Purpose |
|----------|---------|
| RAX | Syscall number |
| RDI | Argument 1 |
| RSI | Argument 2 |
| RDX | Argument 3 |
| R10 | Argument 4 (not RCX — SYSCALL clobbers it) |
| R8 | Argument 5 |
| R9 | Argument 6 |
| RAX (return) | Return value |

The `ulib` crate wraps these conventions in safe Rust functions.

## Fault handling

When a user task causes a hardware exception (page fault on unmapped memory, GPF, divide by zero):

1. The kernel logs the fault details.
2. If the task's parent set a fault endpoint via `SetFaultEp`, a `FaultEvent` message is sent on that endpoint containing the faulting task ID, exception vector, and faulting address. This lets a supervisor (like an init process) detect and respond to child crashes.
3. The task's exit code is set to a fault code (high bit set + exception vector number).
4. The task is killed (set to Zombie).

Exit codes with the high bit set (`0x8000_0000_0000_0000`) indicate a hardware fault rather than a clean `sys_exit`. The low bits carry the x86 exception vector:
- `FAULT_DIVIDE_BY_ZERO` = `0x8000_0000_0000_0000` (vector 0)
- `FAULT_GPF` = `0x8000_0000_0000_000D` (vector 13)
- `FAULT_PAGE_FAULT` = `0x8000_0000_0000_000E` (vector 14)
