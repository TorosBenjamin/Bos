# Memory Management

## Physical memory

Physical memory is tracked in a `NoditMap<u64, Interval<u64>, MemoryType>` — an interval map where each range of page-frame numbers is tagged with its usage type:

- **Usable** — free for allocation
- **UsedByLimine** — bootloader-reserved
- **UsedByKernel(reason)** — page tables, heap pages, kernel stacks, framebuffer
- **UsedByUserMode** — user task frames (demand-paged or ELF-mapped)
- **SharedBuffer** — shared buffer frames (owned by the registry, not per-task)

Allocation is a linear scan for the first usable interval containing a free 4 KiB frame. On free, the frame's type is checked — freeing a frame with the wrong type panics. This catches double-frees and use-after-free at the physical layer.

The explicit type tagging exists because different memory types have different ownership semantics. SharedBuffer frames, for instance, must not be freed when a task exits — they're owned by the shared buffer registry and freed only on explicit `destroy_shared_buf`. Without type tags, task cleanup would accidentally free shared memory that other tasks are still using.

## Higher Half Direct Map (HHDM)

Limine maps all physical memory into a contiguous virtual region starting at the HHDM offset (typically `0xFFFF_8000_0000_0000`). To access physical address `P`, the kernel reads virtual address `P + hhdm_offset`.

This avoids per-access page table manipulation: the kernel can read/write any physical frame just by adding the offset. Page table entries, task stacks, IPC buffers — anything backed by physical memory — are accessed this way.

The HHDM mapping uses 2 MiB huge pages where alignment permits and 4 KiB pages at boundaries, reducing TLB pressure for the common case.

## Virtual address allocation

The `VirtualMemoryAllocator` manages free virtual address ranges using a `NoditSet` (interval set). Two separate allocators exist:

- **Kernel allocator**: ranges starting at `HIGHER_HALF_START` (`0xFFFF_8000_0000_0000`)
- **User allocator** (per-task): ranges from `USER_MIN` (`0x1000`) to `USER_MAX` (`0x7FFF_FFFF_FFFF`)

Allocation finds the first gap large enough for the requested contiguous range, page-aligns both ends, and marks it as occupied. The gap-based approach is simple and avoids fragmentation for the typical OS workload of a few large allocations.

`USER_MIN` starts at `0x1000` (not `0x0`) so that null pointer dereferences in user code fault immediately instead of silently reading the zero page.

## User VMAs (Virtual Memory Areas)

Each task tracks its user-space mappings in a `NoditMap<u64, Interval<u64>, VmaEntry>`. A VMA entry records:

- **Flags** — readable, writable, executable (from ELF or mmap)
- **Backing type**:
  - `EagerlyMapped` — physical frames are installed at creation time (ELF LOAD segments, shared buffers). A not-present fault on an eagerly-mapped page means a bug or attack.
  - `Anonymous` — zero-fill on demand (mmap, user stacks). A not-present fault triggers demand paging.

The backing type distinction is important because it determines what the page fault handler should do: demand-fill an anonymous page, or kill the task for accessing an eagerly-mapped page that somehow lost its mapping.

## Demand paging

When a user task accesses an unmapped anonymous page:

1. The CPU raises a page fault (not-present, from ring 3).
2. The page fault handler reads CR2 (faulting address) and looks up the VMA.
3. If the VMA is `Anonymous`, the handler allocates a zeroed physical frame and installs it in the page table.
4. If allocation fails (OOM), the task is killed.
5. The handler returns via `iretq` and the faulting instruction retries transparently.

A race can occur on SMP: two CPUs might fault on the same page simultaneously. The second CPU's `map_to()` call returns `PageAlreadyMapped`, which is treated as success — the first CPU already installed the frame.

### Prefaulting for kernel access

When the kernel needs to read/write user memory (e.g., copying a syscall argument), it calls `prefault_user_range()` to ensure all pages in the range are present. Without this, the kernel would take a page fault while holding locks or in a context where faults aren't expected.

Prefaulting walks each page in the range, checks if it's present in the page table, and demand-fills any anonymous pages that aren't. This happens under the VMA read lock, which prevents concurrent `munmap` from pulling pages out from under the kernel.
