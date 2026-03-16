# Architecture

Bos is a monolithic kernel — all subsystems (memory, scheduling, IPC, drivers) run in ring 0 in the same address space. This avoids the IPC overhead of a microkernel while keeping the code modular through Rust's module system.

## Core subsystems

| Subsystem | Key files | Purpose |
|-----------|----------|---------|
| Memory | `memory/` | Physical frame allocator, virtual address allocator, page tables, demand paging, HHDM |
| Tasks | `task/` | Task lifecycle, per-CPU local scheduler, global task table |
| Interrupts | `interrupt/` | IDT, naked handlers for timer/page-fault/GPF, NMI panic broadcast |
| Syscalls | `raw_syscall_handler.rs`, `syscall_handlers/` | SYSCALL/SYSRET entry, 44 syscall handlers |
| IPC | `ipc/` | Bounded message-passing channels with blocking send/recv |
| Drivers | `drivers/` | PS/2 keyboard and mouse, IDE/ATA disk (DMA + PIO), PCI enumeration |
| Graphics | `graphics/` | Framebuffer display with ownership model |
| Time | `time/` | TSC calibration, LAPIC timer (TSC-deadline mode), wall-clock from RTC |

## GDT layout

The Global Descriptor Table is ordered specifically for SYSCALL/SYSRET compatibility:

| Index | Offset | Segment |
|-------|--------|---------|
| 0 | 0x00 | Null |
| 1 | 0x08 | Kernel Code (ring 0) |
| 2 | 0x10 | Kernel Data (ring 0) |
| 3 | 0x18 | User Data (ring 3) |
| 4 | 0x20 | User Code (ring 3) |
| 5-6 | 0x28 | TSS (16 bytes) |

User Data must come before User Code because SYSRET derives segment selectors from the STAR MSR by adding fixed offsets: `SS = sysret_base + 8`, `CS = sysret_base + 16`. With `sysret_base = 0x10`, this produces SS=0x1B and CS=0x23 (with RPL=3 bits set), which only works if Data is at 0x18 and Code is at 0x20.

The GDT and TSS are per-CPU because each CPU needs its own TSS (for per-task RSP0 and IST stacks). The IST stack used for exception handlers is intentionally leaked (never freed) since it must remain mapped for the kernel's entire lifetime.

## Address space model

```
0x0000_0000_0000 - 0x0000_0000_0FFF   Null guard (unmapped, catches null derefs)
0x0000_0000_1000 - 0x7FFF_FFFF_FFFF   User space (private per-task)
    ELF segments                        Eagerly mapped from ELF LOAD headers
    mmap regions                        Demand-paged (zero-fill on first access)
    Shared buffers                      Eagerly mapped, same physical frames in multiple tasks
    User stack                          Top of lower half, demand-paged

0xFFFF_8000_0000_0000 - ...            Higher Half Direct Map (HHDM)
    All physical memory                 Linearly mapped at HHDM offset (from Limine)
    Kernel image, stacks, heap          Accessible from any address space
```

User page tables clone the kernel's higher-half L4 entries (256-511) from the kernel's own L4 table. This means kernel code, stacks, and data structures are accessible during interrupts and syscalls without a CR3 switch. Only user-space entries (L4 0-255) differ between tasks.

## Per-CPU data and GS

Each CPU has a `CpuLocalData` struct containing its GDT, IDT, TSS, local APIC interface, run queue, and various flags. This struct is accessed via the GS segment register: the kernel sets `GS_BASE` to point to the current CPU's `CpuLocalData`, and naked handlers use `gs:[offset]` for fast per-CPU access without array indexing.

On ring transitions (interrupt from user mode, syscall), `swapgs` exchanges `GS_BASE` with `KernelGsBase`. `KernelGsBase` is set to 0 so user code cannot access kernel per-CPU data through GS.

## Synchronization

The kernel uses spin-based primitives throughout (no sleeping locks):

- **`spin::Mutex`** — most shared state (task inner, IPC channels, physical memory map)
- **`spin::RwLock`** — VMA lock on tasks (many concurrent syscall readers, rare munmap/mprotect writers)
- **`AtomicU8/U32/U64`** — task state transitions (CAS for Sleeping->Ready), CPU ready counts, flags
- **`spin::Once`** — one-time initialization (IDT, GDT, syscall table, memory allocator)
- **`Arc<Task>`** — reference-counted task sharing across CPUs and wait queues

Spin locks are appropriate here because the kernel runs with interrupts disabled during critical sections (SFMASK disables IF during syscalls; handlers disable interrupts explicitly), so lock holders can't be preempted.
