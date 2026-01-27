# Page Tables

Bos manages x86_64 4-level page tables to map virtual addresses to physical addresses.

The implementation handles:
- Page mapping and unmapping.
- Support for different page sizes (4KiB, 2MiB, 1GiB).
- Page fault handling.
