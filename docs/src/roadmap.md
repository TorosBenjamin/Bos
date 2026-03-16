# Roadmap

This roadmap tracks the evolution of Bos from a bare-metal kernel to a functional desktop OS with windowed applications, networking, and a filesystem.

## Design decisions

These decisions guided the architecture:

- **Init process model**: the kernel spawns one "init" user task, which loads and spawns all other tasks (servers, drivers, apps) from boot modules or the filesystem.
- **Display server + compositor**: only one designated task owns the framebuffer. Other tasks communicate with it via IPC to create windows and submit pixel buffers.
- **User-space drivers**: hardware drivers (e.g., e1000 NIC) run as user tasks with PCI BAR mapping and DMA allocation syscalls, keeping driver bugs out of ring 0.
- **Service discovery**: tasks register named services (IPC endpoints) so clients can find them by name without hardcoded IDs.
- **Shared memory for bulk data**: window pixel buffers and filesystem file contents use shared buffers (zero-copy) rather than IPC message copying.

---

## Phase 1: Spawn syscall

**Status: Done**

One user task can create another from ELF bytes in its own memory. The child gets its own page table, stack, and VMA set. The kernel validates the ELF pointer range in the caller's address space and returns the child's task ID.

---

## Phase 2: Message passing IPC

**Status: Done**

Bounded, unidirectional channels with send/recv endpoints. Blocking and non-blocking variants. 4 KiB max message size, configurable capacity (default 16, max 256). Per-task endpoint tracking for cleanup on exit. `WaitForEvent` syscall for multiplexed waiting across channels, keyboard, mouse, and timeouts.

---

## Phase 3: Display ownership

**Status: Done**

Graphics syscalls restricted to a single display owner (tracked via `AtomicU64`). `TransferDisplay` syscall for ownership handoff. `GetDisplayInfo` unrestricted for dimension queries.

---

## Phase 4: Display server + compositor

**Status: Done**

A user-space display server (`display_server/`) that:

- Registers as the `"display"` service.
- Manages windows via an IPC protocol (`WindowMessageType` enum): `CreateWindow`, `UpdateWindow`, `CloseWindow`, `MoveWindow`, `ResizeWindow`, `RaiseWindow`, `LowerWindow`, `HideWindow`, `ShowWindow`, and `CreatePanel`.
- Allocates shared buffers for window pixel backing stores — clients write pixels directly, then send `UpdateWindow` with a dirty rect. No pixel data flows through IPC messages.
- Composites all visible windows onto the framebuffer, with z-ordering, tiling layout, floating windows, and panel anchoring (top/bottom/left/right).
- Delivers input events to the focused window: key presses, mouse moves, mouse button press/release, focus gained/lost, configure (resize), and frame-presented sync.
- Supports window flags: `WINDOW_FLAG_FLOATING`, `WINDOW_FLAG_ALPHA` (premultiplied alpha compositing), `WINDOW_FLAG_HIDDEN`.
- Loads configuration from `/bos_ds.conf` on the FAT32 filesystem (with fallback defaults).
- Supports drag-to-move and drag-to-resize for floating windows.
- Parent/child window relationships (child always floats).

---

## Phase 5: Shared memory + threads

**Status: Done**

- `CreateSharedBuf` / `MapSharedBuf` / `DestroySharedBuf` syscalls for zero-copy memory sharing.
- `ThreadCreate` syscall for threads sharing the parent's address space (same CR3).
- `Mprotect` and `Mremap` syscalls for changing page permissions and resizing mappings.
- `AllocDma` syscall for physically-contiguous DMA buffer allocation (used by user-space drivers).
- `MapPciBar` / `PciConfigRead` / `PciConfigWrite` syscalls for user-space PCI device access.

---

## Phase 6: Filesystem

**Status: Done**

A user-space FAT32 filesystem server (`fs_server/`):

- Registers as the `"fatfs"` service.
- Reads the IDE disk via `BlockReadSectors` / `BlockWriteSectors` syscalls.
- Serves file read requests over IPC — returns file contents via shared buffers for zero-copy access.
- Init task loads applications from the filesystem instead of embedding them as boot modules.

---

## Phase 7: Networking

**Status: Done**

A user-space network stack:

- **e1000 driver** (`userspace/drivers/e1000/`): user-space Intel e1000 NIC driver using PCI BAR mapping and DMA buffers. Registers as the `"e1000"` service.
- **Net server** (`userspace/servers/net_server/`): registers as the `"net"` service. Provides TCP connect, send, recv, close, and DNS resolve operations over IPC.
- **Client libraries**: `ulib::net` for raw network access, `http_client` for HTTP requests (including TLS), `html_renderer` for HTML parsing.

---

## Phase 8: Applications

**Status: In progress**

User-space applications running on top of the display server:

- **Boser** (`userspace/apps/boser/`): web browser using `http_client` + `html_renderer`.
- **Launcher** (`userspace/apps/launcher/`): application launcher panel (hidden by default, toggled by Super+Space).
- **Files** (`userspace/apps/files/`): file manager.
- **Hello egui** (`userspace/apps/hello_egui/`): egui demo app via the `bos_egui` framework.
- **utest** (`userspace/apps/utest/`): integration test runner.

### Frameworks

- **ulib** — core userspace library: syscall wrappers, window client, filesystem client, network client.
- **bos_std** — higher-level standard library (networking abstractions).
- **bos_egui** — egui integration for Bos (renders via shared-buffer windows).
- **bos_image** — image decoding for userspace.

---

## Dependency graph

```
Phase 1: Spawn
   |
   v
Phase 2: IPC
   |
   +------------------+------------------+
   v                  v                  v
Phase 3: Display   Phase 5: Shared    Phase 6: Filesystem
  ownership          memory/threads       |
   |                  |                   |
   +--------+---------+                   |
            v                             |
     Phase 4: Display                     |
       server                             |
            |                             |
            +----------+------ -----------+
                       v
                Phase 7: Networking
                       |
                       v
                Phase 8: Applications
```

---

## Future work

- **Write support in filesystem** — currently read-only FAT32.
- **Full compositor** — double-buffered per-window framebuffers, alpha blending improvements, window decorations/title bars.
- **Process management** — signal delivery, process groups, job control.
- **Audio** — sound driver and audio mixing server.
- **More hardware support** — USB, NVMe, AHCI.
- **Memory-mapped files** — `mmap` backed by filesystem pages.
- **Async I/O** — `poll`/`epoll`-style multiplexing across file descriptors and IPC channels.
