# Interrupts

Interrupt handling is a core part of the kernel.

## IDT (Interrupt Descriptor Table)

The IDT is initialized in `kernel/src/interrupt/idt.rs`. It sets up handlers for:
- Standard x86 exceptions (Division by zero, Page Fault, Double Fault, etc.).
- Hardware interrupts (Timer, Keyboard, etc.).

## Handlers

Handlers are implemented in `kernel/src/interrupt/handlers.rs`. 
Some handlers (like the timer) use `naked_asm!` to manually save and restore CPU state to facilitate context switching.

## NMI (Non-Maskable Interrupts)

NMIs are used for cross-CPU communication, such as notifying other CPUs when a kernel panic occurs so they can also stop safely.
