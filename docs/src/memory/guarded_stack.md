# Guarded Stacks

Every kernel stack and exception stack uses a guard page: a single unmapped page at the bottom of the stack region. If code overflows the stack, it hits the guard page and triggers a page fault with a clear "stack overflow" panic instead of silently corrupting adjacent memory.

## Layout

```
Low address   [Guard page - unmapped]     <- stack overflow hits here
              [Stack page N]              <- stack grows downward
              [Stack page N-1]
              ...
High address  [Stack page 0]             <- stack top (RSP starts here)
```

## Sizes

- **Normal kernel stack**: 256 KiB (64 pages) — used for all kernel and user tasks. This is large enough for deep call chains through syscall handlers, IPC, and memory management without risking overflow.
- **Exception handler stack**: 64 KiB (16 pages) — used via IST for page faults and double faults. Smaller because exception handlers have shallower call depth.

## Allocation

1. Request a contiguous virtual range of `guard_pages + stack_pages` from the virtual address allocator.
2. Leave the first page unmapped (guard).
3. Allocate physical frames and map them for the remaining pages.

The guard page address is recorded in a global `BTreeMap<Page, StackInfo>`, so the page fault handler can identify a guard page hit and report it as a stack overflow with the stack's owner information.

## Cleanup

`GuardedStack` implements `Drop`: it unmaps all stack pages, frees the backing physical frames, and releases the virtual address range. This ensures no leaks when tasks exit.

Exception handler stacks (IST stacks in the TSS) are an exception — they're intentionally leaked because they must remain mapped for the entire kernel lifetime. Freeing them would leave the CPU without a valid IST stack for double faults.

## Why not a larger guard region

A single guard page is sufficient because x86_64 stack accesses are sequential (push/call decrement RSP by 8 bytes). There's no way to "jump over" the guard page with normal stack operations. Large stack-allocated arrays could theoretically skip the guard, but Rust's stack probe mechanism (enabled by default) inserts probes for large `alloca`-style allocations.
