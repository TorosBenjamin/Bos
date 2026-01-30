# Architecture

Bos follows a monolithic kernel design, though it is organized into several modular components.

## Core Components

- **Memory Management**: Handles physical memory allocation, page table management, and virtual memory allocation.
- **Interrupt Handling**: Manages the IDT, GDT, and handles various hardware and software interrupts.
- **Task Management**: Implements a preemptive multitasking system with local and global schedulers. Supports both kernel-mode and user-mode tasks.
- **System Calls**: Provides the interface for userland applications to interact with the kernel via SYSCALL/SYSRET.
- **Graphics**: Basic frame-buffer based graphics support.
- **Time**: TSC and LAPIC timer based timekeeping.

## GDT Layout

The Global Descriptor Table is ordered for compatibility with SYSCALL/SYSRET:

| Index | Offset | Segment |
|-------|--------|---------|
| 0 | 0x00 | Null |
| 1 | 0x08 | Kernel Code (ring 0) |
| 2 | 0x10 | Kernel Data (ring 0) |
| 3 | 0x18 | User Data (ring 3) |
| 4 | 0x20 | User Code (ring 3) |
| 5-6 | 0x28 | TSS (16 bytes) |

The User Data and User Code segments must appear in this order (data before code) for SYSRET to derive the correct selectors from the STAR MSR.

## Address Space Model

- **Kernel**: Mapped in the higher half (L4 entries 256-511), shared across all address spaces.
- **User**: Mapped in the lower half (L4 entries 0-255), private per task.
- User page tables clone the kernel's higher-half entries from the kernel L4, so kernel code, stacks, and data structures remain accessible during interrupts and syscalls.
