# Memory Management

Bos implements a comprehensive memory management system.

## Components

- **Physical Memory Allocation**: Managed in `kernel/src/memory/physical_memory.rs`.
- **Virtual Memory Allocation**: Managed in `kernel/src/memory/vaddr_allocator.rs`.
- **Page Table Management**: Handles the x86_64 4-level page tables.
- **Global Allocator**: Provides `alloc` support for the kernel.
- **CPU Local Data**: Stores per-CPU information.
- **Guarded Stacks**: Provides stack overflow protection using guard pages.

## Higher Half Direct Map (HHDM)

Bos utilizes the HHDM feature provided by Limine to map all physical memory into a specific region of the virtual address space.
