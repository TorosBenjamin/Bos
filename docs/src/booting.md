# Booting

Bos uses the [Limine](https://limine-bootloader.org/) bootloader and the Limine boot protocol.

## Boot Sequence

1. **Limine Entry**: Limine loads the kernel binary into memory and jumps to the entry point.
2. **Kernel Main**: The `kernel_main` function in `kernel/src/main.rs` is called.
3. **Early Initialization**:
    - Initialize the display frame buffer.
    - Initialize the logger.
    - Initialize the memory map and physical memory allocator for the BSP (Bootstrap Processor).
4. **BSP Initialization**:
    - Initialize GDT and IDT.
    - Parse ACPI tables.
    - Initialize APIC and local APIC.
    - Calibrate timers (TSC, LAPIC).
    - Initialize the run queue and spawn initial tasks.
5. **Interrupts**: Enable interrupts and start the scheduler.

## Multi-Processor (MP) Support

The kernel supports multi-processor systems. APs (Application Processors) are started by the BSP and follow a similar initialization path as the BSP but skip global initializations.
