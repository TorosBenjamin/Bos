# Booting

Bos uses the [Limine](https://limine-bootloader.org/) boot protocol. Limine sets up long mode, identity-maps physical memory into the Higher Half Direct Map (HHDM), loads the kernel ELF, and provides structured responses (memory map, framebuffer, boot modules, RSDP pointer, SMP info).

## BSP boot sequence

The Bootstrap Processor (BSP) runs `kernel_main()`, which initializes subsystems in a carefully ordered sequence — each step depends on the previous ones:

1. **Display + Logger** — framebuffer from Limine response, serial output on COM1 (0x3F8). Logging works from this point on. Timestamps are initially zero until TSC is calibrated.

2. **Physical memory** — parses Limine's memory map into a `NoditMap` of intervals tagged by type (Usable, UsedByLimine, UsedByKernel, etc.). This is the source of truth for all frame allocation.

3. **Global allocator** — claims up to 4 MiB from the largest usable physical regions for the kernel heap. Uses the Talc allocator. After this, `Vec`, `Box`, `Arc`, and friends work.

4. **NMI handler state** — marks this CPU as ready to receive NMIs (for panic broadcast).

5. **Switch to guarded kernel stack** — Limine's initial stack has no guard page. The kernel allocates a `GuardedStack` (256 KiB + 1 guard page) and switches to it before doing anything that might recurse deeply.

6. **GDT + IDT** — per-CPU tables. The GDT includes kernel/user segments and a TSS with an IST stack for exception handlers. The IDT wires up exception handlers (page fault, GPF, double fault) and device interrupts (timer, keyboard, mouse, ATA).

7. **ACPI** — parses the RSDP (from Limine) to find the MADT, which describes the interrupt model: local APIC addresses, I/O APIC addresses, and interrupt source overrides (e.g., IRQ1 might be remapped to GSI 1 with specific polarity).

8. **APIC** — initializes the local APIC (x2apic if supported, falling back to xapic MMIO). Sets spurious, error, and timer interrupt vectors. The LAPIC timer is configured in TSC-deadline mode.

9. **I/O APIC** — maps the I/O APIC's MMIO registers, stores interrupt source overrides, and enables IRQs for keyboard (IRQ1), mouse (IRQ12), and primary ATA (IRQ14). All other pins are masked.

10. **TSC calibration** — uses PIT channel 2 as a reference clock: counts TSC ticks over a 10ms PIT interval to determine `ticks_per_ms`. This value is used for all subsequent time calculations.

11. **Wall clock** — reads the RTC (CMOS) to get the current date/time, converts to Unix nanoseconds, and anchors the TSC to this epoch. After this, `tsc::now_ns()` returns wall-clock time.

12. **SYSCALL MSRs** — configures LSTAR (entry point), STAR (segment selectors), and SFMASK (mask IF during syscall). This is per-CPU.

13. **Run queue + tasks** — initializes the per-CPU run queue, spawns an idle task (kernel mode, lowest priority, runs `hlt` in a loop), and spawns the init task from the ELF boot module (user mode, high priority).

14. **PS/2 mouse init** — enables the aux port, IRQ12, and stream mode. Drains the ACK byte via polling before interrupts are enabled (otherwise the pending IRQ12 would misalign the 3-byte packet accumulator).

15. **Enable interrupts** — `sti`. The LAPIC timer fires, the scheduler starts, and the init task begins running.

## AP boot sequence

Application Processors (APs) are started via Limine's multiprocessor protocol. Each AP follows a similar path but skips global initialization:

1. Switch CR3 to the kernel page table (same as BSP — all CPUs share the kernel address space).
2. Allocate per-CPU `CpuLocalData`, set up GS_BASE.
3. Switch to a guarded kernel stack.
4. Initialize per-CPU GDT, IDT, local APIC, LAPIC timer, SYSCALL MSRs.
5. Initialize per-CPU run queue, spawn per-CPU idle task.
6. Enable interrupts and enter the idle loop.

APs do not re-parse ACPI, re-initialize the I/O APIC, or spawn user tasks — those are global one-time operations handled by the BSP.

## Panic handling

When a panic occurs:

1. An atomic `DID_PANIC` flag is set via CAS — only the first panicking CPU proceeds.
2. The panic handler attempts to identify the running task (using `try_lock()` on the run queue to avoid deadlocking if the panic happened while holding that lock).
3. The framebuffer is taken over and a panic screen is drawn (dark blue background with the panic message and backtrace).
4. An NMI is sent to every other CPU. Each CPU's NMI handler checks `NmiHandlerState` — if it's `NmiHandlerSet`, it transitions to `KernelPanicked` and enters an infinite `hlt` loop.

This ensures all CPUs stop cleanly even if they're in the middle of work. The NMI handler state machine prevents cascading NMI storms (a CPU that's already panicked won't re-send NMIs).
