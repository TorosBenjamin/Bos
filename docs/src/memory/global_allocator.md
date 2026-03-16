# Global Allocator

The kernel heap uses the [Talc](https://crates.io/crates/talc) allocator with an `ErrOnOom` policy (panics on allocation failure). It provides standard Rust `alloc` support: `Vec`, `Box`, `Arc`, `String`, etc.

## Initialization

During BSP boot, the allocator claims physical frames to build a 4 MiB heap:

1. Sort all usable memory regions by size (largest first).
2. Claim frames from the largest regions until 4 MiB is reached (up to 16 regions).
3. Map claimed frames into the HHDM and hand the memory to Talc.

Largest-first selection minimizes fragmentation of the physical memory map — it's better to consume one large region than to fragment many small ones. If the system has less than 4 MiB of usable memory, the allocator initializes with whatever is available and logs a warning.

## Why 4 MiB

4 MiB is enough for the kernel's dynamic data structures (task tables, IPC channels, VMA maps, PCI device lists) without being wasteful. The kernel doesn't allocate large buffers on the heap — bulk data (framebuffers, DMA buffers, user memory) uses dedicated physical frame allocation instead.

## Why panic on OOM

In a kernel, running out of heap memory is a fatal condition — most code paths don't have meaningful recovery strategies for allocation failure. Panicking immediately with a clear message is more debuggable than propagating errors through dozens of call sites that would all end up panicking anyway.
