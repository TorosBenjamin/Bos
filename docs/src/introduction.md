# Introduction

Bos is a hobbyist operating system written in Rust, targeting x86_64. It boots via Limine, runs user tasks in ring 3 with separate address spaces, and provides IPC, demand-paged virtual memory, SMP scheduling, PS/2 input, a tiling/floating compositor, FAT32 filesystem, TCP/IP networking, and a web browser — all running as cooperating user-space tasks on top of a monolithic kernel.

## What makes it interesting

- **Rust `no_std` kernel** — uses Rust's type system (RAII, `Arc`, `Mutex`, enums) to manage kernel resources, with `unsafe` confined to hardware interaction and a few carefully audited spots (pointer validation, naked asm handlers).
- **Demand paging** — user memory is lazily allocated on first access, reducing boot time and memory pressure.
- **Per-CPU scheduling with starvation prevention** — each CPU runs its own priority-based scheduler, with skip-count thresholds that boost starved tasks.
- **TOCTOU-safe user pointer handling** — an `RwLock`-based guard system prevents concurrent `munmap` from invalidating pointers the kernel is actively using.
- **Naked interrupt/syscall handlers** — hand-written assembly entry points control register save/restore precisely, avoiding compiler-generated prologues that would break context switching.

## Project structure

```
kernel/core/                    Kernel source (entry point, subsystems, drivers)
shared/kernel_api_types/        Shared types (syscall numbers, IPC codes, window protocol)
userspace/
  servers/
    init_task/                  First user process — spawns all other tasks
    display_server/             Tiling/floating compositor with window management
    fs_server/                  FAT32 filesystem server
    net_server/                 TCP/IP network stack
  drivers/
    e1000/                      User-space Intel e1000 NIC driver
  frameworks/
    ulib/                       Core userspace library (syscall wrappers, clients)
    bos_std/                    Higher-level standard library
    bos_egui/                   egui integration for Bos
  libs/
    http_client/                HTTP client with TLS
    html_renderer/              HTML parser and renderer
    bos_image/                  Image decoding
  apps/
    boser/                      Web browser
    launcher/                   Application launcher panel
    files/                      File manager
    hello_egui/                 egui demo
    user_land/                  Visual demos (bouncing cubes)
    utest/                      Integration test runner
tools/runner/                   Build-and-run helper (QEMU launcher)
tests/                          Kernel integration tests
docs/                           This documentation (mdBook)
```
