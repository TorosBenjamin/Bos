# Userland

Bos supports running userland applications in ring 3 with their own address spaces.

## User Task Creation

User tasks are created from ELF binaries loaded as Limine modules. The function `create_user_task_from_elf()` in `kernel/src/user_task_from_elf.rs` handles the full setup:

1. **Parse ELF**: The ELF binary is read from the Limine module matching `INIT_TASK_PATH`.
2. **Create address space**: A new user L4 page table is allocated. The kernel's higher-half entries (L4[256..511]) are cloned into it, ensuring kernel code and data remain accessible during interrupts and syscalls.
3. **Map ELF segments**: LOAD segments are mapped into the user address space at their specified virtual addresses. BSS regions are allocated and zeroed.
4. **Allocate user stack**: A 64 KiB stack is mapped at the top of the user address space (just below `LOWER_HALF_END`).
5. **Create Task**: A `Task::new_user()` is constructed with the ELF entry point, user stack top, the page table, and user CS/SS selectors from the GDT.

The returned `Task` is then passed to `spawn_task()` and enters user mode via the scheduler's normal iretq path -- no special sysretq transition is needed.

## Address Space Layout

| Region | Address Range | Description |
|--------|--------------|-------------|
| User code/data | ELF-defined | Mapped from ELF LOAD segments |
| User stack | Below `0x800000000000` | 64 KiB, grows downward |
| Kernel (shared) | `0xFFFF800000000000`+ | Higher-half direct map + kernel image |

## Interrupt Handling from Ring 3

When a timer interrupt fires while a user task is running:

1. The CPU reads RSP0 from the TSS and switches to the task's kernel stack.
2. The CPU pushes the iretq frame (user SS, RSP, RFLAGS, CS, RIP).
3. The timer handler pushes 15 GPRs.
4. The scheduler saves the kernel stack RSP and may switch to a different task.
5. When this task is resumed, GPRs are popped and iretq returns to ring 3.

Each user task has its own kernel stack, so interrupt state is preserved across context switches.

## Shared Types

The `kernel_api_types` crate provides shared definitions (like syscall numbers and structures) that both the kernel and userland applications use to communicate.
