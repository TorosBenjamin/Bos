# Introduction

Bos is a hobbyist operating system kernel written in Rust, targeting x86_64. It boots via Limine, runs user tasks in ring 3 with separate address spaces, and provides IPC, demand-paged virtual memory, SMP scheduling, and PS/2 input — enough to host a multi-window display server with cooperating user tasks.

## What makes it interesting

- **Rust `no_std` kernel** — uses Rust's type system (RAII, `Arc`, `Mutex`, enums) to manage kernel resources, with `unsafe` confined to hardware interaction and a few carefully audited spots (pointer validation, naked asm handlers).
- **Demand paging** — user memory is lazily allocated on first access, reducing boot time and memory pressure.
- **Per-CPU scheduling with starvation prevention** — each CPU runs its own priority-based scheduler, with skip-count thresholds that boost starved tasks.
- **TOCTOU-safe user pointer handling** — an `RwLock`-based guard system prevents concurrent `munmap` from invalidating pointers the kernel is actively using.
- **Naked interrupt/syscall handlers** — hand-written assembly entry points control register save/restore precisely, avoiding compiler-generated prologues that would break context switching.

## Project structure

```
kernel/core/        Kernel source (entry point, subsystems, drivers)
shared/kernel_api_types/  Shared types and constants (syscall numbers, IPC codes, event structs)
init_task/          First user process — spawns other tasks
display_server/     Display server user task (compositor)
ulib/               User-space library wrapping syscalls
tools/runner/       Build-and-run helper (QEMU launcher)
tests/              Integration tests
docs/               This documentation (mdBook)
```
