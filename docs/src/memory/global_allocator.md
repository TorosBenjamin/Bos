# Global Allocator

The kernel uses a global allocator to support dynamic memory allocation (`Vec`, `Box`, `Arc`, etc.).

It is initialized during the BSP boot process after the physical memory allocator is ready.
