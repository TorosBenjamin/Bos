# Bos OS Architecture

Bos is a monolithic x86-64 operating system written in Rust. It boots via
Limine (UEFI + BIOS), runs a preemptive multi-core kernel, and provides a
userspace with IPC-based servers for display, filesystem, and networking.

## Component Map

```
┌─────────────────────────────────────────────────────────────────────┐
│                          Userspace                                  │
│                                                                     │
│  ┌─────────┐  ┌──────────┐  ┌──────────┐  ┌────────┐  ┌────────┐ │
│  │Launcher │  │hello_egui│  │  files   │  │ boser  │  │ utest  │ │
│  └────┬────┘  └────┬─────┘  └────┬─────┘  └───┬────┘  └───┬────┘ │
│       │  bos_egui  │    ulib     │    ulib     │    ulib   │      │
│  ┌────┴────────────┴─────────────┴─────────────┴───────────┴────┐ │
│  │                    ulib  (syscall wrappers)                   │ │
│  │         window · display · fs · net · bench_harness           │ │
│  └──────────────────────────┬────────────────────────────────────┘ │
│                             │ IPC (channels + shared buffers)      │
│  ┌──────────────┐  ┌───────┴──────┐  ┌──────────┐  ┌───────────┐ │
│  │Display Server│  │  fs_server   │  │net_server│  │IDE / e1000│ │
│  │  (compositor)│  │   (FAT32)    │  │  (TCP)   │  │ (drivers) │ │
│  └──────┬───────┘  └──────┬───────┘  └────┬─────┘  └─────┬─────┘ │
│         │                 │               │               │       │
└─────────┼─────────────────┼───────────────┼───────────────┼───────┘
          │   syscall       │               │               │
┌─────────┴─────────────────┴───────────────┴───────────────┴───────┐
│                          Kernel                                    │
│                                                                    │
│  Syscall dispatch · Task scheduler · IPC channels · Shared buffers │
│  Physical memory · Page tables · HHDM · Guarded stacks             │
│  LAPIC timer · IDT · GDT · ACPI · x2APIC · Per-CPU data (GS.Base)│
│  PCI · IDE PIO · Keyboard · Mouse                                  │
│                                                                    │
│  Scheduling policies: RoundRobin, Priority, IpcAware               │
└────────────────────────────────────────────────────────────────────┘
          │
   Limine bootloader (UEFI + BIOS)
          │
      x86-64 hardware
```

## Documentation

### Handwritten Architecture Docs

These cover design decisions, protocols, and invariants that cannot be
expressed in API docs alone.

| Document | Covers |
|----------|--------|
| [Kernel](../kernel/core/.claude/CLAUDE.md) | Build, toolchain, memory layout, scheduling, context switching, syscall convention, GDT, known issues |
| [Display Server](display_server.md) | Compositor architecture, window lifecycle, IPC protocol, tiling layout, input handling, rendering pipeline |
| [Filesystem Server](fs_server.md) | FAT32 driver, IPC protocol, shared buffer pattern, IDE communication, testing |

### API Reference (`cargo doc`)

Library crates have `//!` crate-level documentation and `///` item docs.
Generate with:

```sh
cargo doc --workspace --no-deps --document-private-items
```

| Crate | Type | Description |
|-------|------|-------------|
| `kernel_api_types` | Shared types | Syscall numbers, IPC/service error codes, graphics types, window/fs/net protocol structs |
| `ulib` | Userspace library | Syscall wrappers, windowing, filesystem client, networking client, benchmarking harness |
| `bos_egui` | GUI framework | Egui abstraction layer (real egui on Linux, software stub on Bos) |
| `fs_server` | Server | FAT32 implementation with `BlockDev` trait (testable via `cargo test -p fs_server`) |

## Key Design Patterns

### IPC with Shared Buffers

Large data (window pixels, file contents) is transferred via shared physical
memory. The kernel's `sys_create_shared_buf` allocates physical pages and maps
them into the calling task. Other tasks can map the same buffer via
`sys_map_shared_buf`. Only metadata (dirty rects, file paths) travels over IPC
channels.

### Service Registry

Servers register with a name (`sys_register_service("fatfs", endpoint)`).
Clients discover them with `sys_lookup_service("fatfs")`. This decouples
startup ordering — clients retry until the service appears.

### Event-Driven I/O

`sys_wait_for_event` blocks a task until any of its watched channels, mouse, or
keyboard has data (or a timeout expires). This avoids busy-polling while still
allowing multi-source wakeup.

### Scheduling Policies

The scheduler is policy-pluggable. Three policies are implemented:

- **RoundRobin** — equal time slices, FIFO order.
- **Priority** — three levels (Background, Normal, High), preemptive.
- **IpcAware** — boosts tasks that just completed an IPC receive, reducing
  round-trip latency under contention.

The active policy is selected at compile time via a type alias in
`local_scheduler.rs`.

## Build & Run

```sh
# Run the full OS in QEMU
LIMINE_PATH=/usr/local/share/limine cargo run -p runner

# Run kernel tests
LIMINE_PATH=/usr/local/share/limine cargo ktest

# Run fs_server unit tests (host)
cargo test -p fs_server

# Run display_server unit tests (host)
cargo test -p display_server

# Generate API docs
cargo doc --workspace --no-deps

# Run benchmarks
python3 -m bench_py --bench-type syscall --scenario 0  # (from os/tools/)
```

## Benchmarking

The benchmark framework (`os/tools/bench_py/` + `userspace/bench/`) measures
kernel performance across four categories:

| Type | Scenarios |
|------|-----------|
| `ipc` | Ping-pong, fan-out, service chain (with configurable background workers) |
| `syscall` | get_ticks, yield, mmap+munmap, get_time_ns, channel lifecycle |
| `ctx_switch` | Yield-based ping-pong between two tasks |
| `mem` | mmap 4K/64K, shared buffer lifecycle, mprotect |

Results are saved as JSON with statistics (mean, median, stddev, percentiles)
and optional matplotlib plots.
