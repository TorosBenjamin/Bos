# Guarded Stacks

To prevent kernel stack overflows from corrupting other memory regions, Bos uses "Guarded Stacks".

A guarded stack consists of:
- A guard page (non-present in page tables) at the bottom of the stack.
- The actual stack memory.

If the kernel exceeds the stack space, it hits the guard page, triggering a Page Fault, which the kernel catches and panics with a clear "Stack overflow" message.
