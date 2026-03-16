# Page Tables

Bos uses x86_64 4-level page tables (PML4 -> PDPT -> PD -> PT) with the `x86_64` crate's `OffsetPageTable` mapper. The HHDM offset lets the mapper translate any page table physical address to a kernel-accessible virtual address without extra mappings.

## User page table creation

When a user task is created, a new L4 (PML4) frame is allocated:

1. The L4 frame is zeroed.
2. Kernel higher-half entries (L4 indices 256-511) are cloned from the kernel's own L4 table. Entry 511 covers the kernel image and HHDM. This sharing means kernel code, stacks, and data are accessible in every address space — no CR3 switch needed for interrupts or syscalls.
3. User-space entries (L4 indices 0-255) start empty and are populated as ELF segments and mmap regions are mapped.

Cloning only the L4 entries (not the full subtree) is sufficient because page tables form a tree of pointers: the L4 entry points to the same L3/L2/L1 subtree that the kernel uses. Changes to kernel mappings in the subtree are automatically visible in all user page tables.

## Page sizes

- **4 KiB pages** — used for user memory, kernel stacks, and fine-grained mappings.
- **2 MiB huge pages** — used in the HHDM mapping where physical memory is 2 MiB-aligned. Reduces TLB entries needed for the direct map.
- **1 GiB huge pages** — supported by the mapping code but not currently used (requires hardware support and aligned regions).

## Page fault flow

```
User instruction faults (not-present)
  -> CPU pushes error code + iretq frame, switches to kernel stack (TSS.RSP0)
  -> Naked page fault handler: swapgs if from ring 3, save registers
  -> Read CR2 (faulting address)
  -> Ring 3 fault: try demand fill
      -> Look up VMA for address
      -> Anonymous VMA: allocate frame, zero it, install PTE, return (retry instruction)
      -> EagerlyMapped or no VMA: kill task
  -> Ring 0 fault: check guard pages (stack overflow), else panic
```

The page fault handler uses a naked wrapper because it needs to perform `swapgs` before any GS-relative access (like reading per-CPU data). The standard `x86-interrupt` calling convention doesn't support this.

## Freeing user address spaces

When a user task is dropped, `free_user_address_space()` walks L4 entries 0-255 recursively, freeing:

- All L3, L2, L1 table frames (allocated by the page mapper)
- All data frames in leaf entries (allocated for ELF segments, mmap, demand paging)

SharedBuffer frames (tagged `MemoryType::SharedBuffer` in the physical allocator) are skipped — they're owned by the shared buffer registry and may still be mapped in other tasks.
