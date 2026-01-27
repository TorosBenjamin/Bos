# Architecture

Bos follows a monolithic kernel design, though it is organized into several modular components.

## Core Components

- **Memory Management**: Handles physical memory allocation, page table management, and virtual memory allocation.
- **Interrupt Handling**: Manages the IDT, GDT, and handles various hardware and software interrupts.
- **Task Management**: Implements a cooperative/preemptive multitasking system with local and global schedulers.
- **System Calls**: Provides the interface for userland applications to interact with the kernel.
- **Graphics**: Basic frame-buffer based graphics support.
- **Time**: TSC and LAPIC timer based timekeeping.
