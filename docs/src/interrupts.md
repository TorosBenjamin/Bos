# Interrupts

## Interrupt vector assignments

| Vector | Source | Handler style |
|--------|--------|--------------|
| 0 | Divide Error (#DE) | Standard exception |
| 3 | Breakpoint (#BP) | Standard exception |
| 8 | Double Fault (#DF) | IST stack, diverging |
| 13 | General Protection Fault (#GP) | Naked (ring check, frame validation) |
| 14 | Page Fault (#PF) | Naked + IST (demand paging, swapgs) |
| 0x20 | LAPIC Spurious | Ignored |
| 0x21 | LAPIC Timer | Naked (context switch) |
| 0x22 | LAPIC Error | Standard |
| 0x23 | Keyboard (IRQ1) | Naked (swapgs + SS fix) |
| 0x24 | Reschedule IPI | Naked (EOI only) |
| 0x25 | Mouse (IRQ12) | Naked (swapgs + SS fix) |
| 0x26 | Primary ATA (IRQ14) | Naked (swapgs + SS fix) |
| NMI | Non-Maskable Interrupt | Panic broadcast |

## Why naked handlers

Most interrupt handlers use `#[unsafe(naked)]` instead of the standard `x86-interrupt` calling convention. There are two reasons:

1. **`swapgs` control** — when an interrupt fires from ring 3, the kernel must execute `swapgs` before any GS-relative access (per-CPU data, task context pointers). The compiler-generated prologue from `x86-interrupt` would access GS before the handler can swap it. Naked handlers check the saved CS register's RPL bits to determine if `swapgs` is needed.

2. **Context switch integration** — the timer handler saves/restores the full register set into a `CpuContext` struct that persists across context switches. The compiler-generated code would push/pop registers on the stack in its own order, incompatible with the CpuContext layout.

## Timer handler (context switching)

The timer handler is the core of preemptive scheduling. Its flow:

1. **Entry**: if from ring 3, `swapgs`.
2. **Load context pointer** from `gs:[current_context_ptr_offset]`.
3. **Null check**: if no task is running (first tick after boot), call `timer_bootstrap_first_task()` to pick the first ready task.
4. **Syscall check**: if `in_syscall_handler` flag is set, the syscall entry point already saved registers to `CpuContext` — skip saving again. This avoids double-saving and lets the timer preempt a sleeping syscall.
5. **Save state**: write all 15 GPRs and the iretq frame (RIP, CS, RFLAGS, RSP, SS) into the current `CpuContext`.
6. **Call scheduler**: `schedule_from_interrupt()` returns a pointer to the next task's `CpuContext`.
7. **Store new context pointer** in GS.
8. **Restore state**: copy the new context's iretq frame to the stack, restore 15 GPRs.
9. **SS RPL fix**: KVM strips RPL bits from SS on hardware push (0x1B becomes 0x18). If returning to ring 3, the handler forces SS to 0x1B to avoid a GPF on `iretq`.
10. **Exit**: if returning to ring 3, `swapgs`. Then `iretq`.

## Page fault handler

The page fault handler uses a naked wrapper for swapgs, then calls an inner function:

- **Ring 3 faults**: look up the VMA for the faulting address (CR2). If it's an anonymous VMA, demand-fill the page. If the VMA doesn't exist or is eagerly-mapped, the task accessed invalid memory — kill it and send a fault notification to the supervisor (if configured via `set_fault_ep`).
- **Ring 0 faults**: check if the faulting address is a known guard page (stack overflow). Otherwise, panic — a kernel page fault on mapped memory is a bug.

The handler uses IST so it gets a clean stack even if the fault occurred due to stack overflow (where the current stack is exhausted).

## General Protection Fault handler

- **Ring 3**: log the fault details and kill the task.
- **Ring 0**: attempt to dump the iretq frame at RSP for debugging. The frame read is guarded by alignment checks, HHDM bounds validation, and page table translation — if RSP is corrupted, these checks prevent a recursive #GP that would escalate to a double fault with no useful information.

## Device interrupt handlers (keyboard, mouse, ATA)

All device handlers follow the same pattern:

1. `swapgs` if from ring 3.
2. Save 9 caller-saved registers (rax, rcx, rdx, rsi, rdi, r8, r9, r10, r11).
3. Call the inner handler (processes the device data, sends EOI to local APIC).
4. Restore 8 registers.
5. Fix SS RPL if returning to ring 3.
6. `swapgs` if returning to ring 3.
7. Pop the last saved register, `iretq`.

Only caller-saved registers are preserved because the inner function (Rust ABI) preserves callee-saved registers itself.

## NMI and panic broadcast

NMIs are used to halt all CPUs when one CPU panics. Each CPU maintains an `NmiHandlerState`:

- `NmiHandlerNotSet` — before IDT is loaded (ignore NMI)
- `NmiHandlerSet` — IDT ready, will respond to panic NMI
- `KernelPanicked` — this CPU received the panic NMI, entering halt loop

The panicking CPU sends NMIs only to CPUs in state `NmiHandlerSet`, preventing NMI storms. A CPU that has already panicked doesn't re-broadcast.

## I/O APIC

The I/O APIC routes external device interrupts to local APICs. During initialization:

1. The I/O APIC's MMIO base address is read from the ACPI MADT.
2. The MMIO page is explicitly mapped (it's device memory, not in the HHDM).
3. Interrupt source overrides from the MADT are recorded (e.g., ISA IRQ 0 might be remapped to GSI 2).
4. All redirection table entries are initially masked.
5. Individual IRQs are unmasked as drivers initialize (keyboard, mouse, ATA).

Each redirection entry maps a GSI (Global System Interrupt) to an interrupt vector and target APIC ID.
