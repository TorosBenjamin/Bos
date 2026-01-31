# Roadmap: Multi-Window Visual Demo

This roadmap tracks the path from the current single-task graphics demo to a multi-window visual demo with cooperating user tasks, mediated by a display server.

## Design Decisions

These decisions guide all phases below:

- **Task hierarchy:** Init process model. The kernel spawns one "init" user task, which uses `Spawn` to create all other user tasks.
- **Display model:** Display server (single owner) + compositor. Only one designated task may call graphics syscalls. Other tasks communicate with it via IPC.
- **Spawn mechanism:** The caller passes a pointer to ELF bytes in its own memory. The kernel parses the ELF and creates a new task.
- **IPC:** Message passing first (kernel-mediated channels), then shared memory later (same physical frames mapped into two tasks at the same vaddr).

---

## Phase 1: Spawn Syscall

**Status:** Done
**Blocked by:** Nothing (all prerequisites exist)

**Goal:** One user task can create another from ELF bytes in its memory.

**What to build:**
- Implement the `Spawn` syscall handler (number 4, already reserved) -- caller passes `(elf_ptr, elf_len)`, kernel parses the ELF, creates a new address space, and schedules the child task
- The child gets its own page table, stack, and vaddr set (like `create_user_task_from_elf` but reading from user memory instead of a Limine module)
- Return a task ID to the parent so it can reference the child later
- Kernel validates that `elf_ptr..elf_ptr+elf_len` is readable in the caller's address space
- Add userland wrapper in `ulib/src/lib.rs`

**Files to modify/create:**
- `kernel/src/syscall_handlers.rs` -- add `sys_spawn`
- `kernel/src/raw_syscall_handler.rs` -- register `Spawn` handler
- `kernel/src/user_task_from_elf.rs` -- extract common logic, add variant that reads from user memory
- `ulib/src/lib.rs` -- add `sys_spawn` wrapper

**Tests:**
- Init task spawns a child task that calls `Exit`. Init continues running.

---

## Phase 2: Message Passing IPC

**Status:** Done
**Blocked by:** Phase 1 (need Spawn to test two tasks communicating)

**Goal:** Two tasks can send and receive fixed-size messages through kernel-mediated channels.

**What to build:**
- New kernel object: **Channel** -- a bounded queue of messages between exactly two endpoints
- Syscalls:
  - `ChannelCreate` -- returns two endpoint IDs (send-end, recv-end)
  - `ChannelSend(endpoint, msg_ptr, msg_len)` -- copies message into kernel buffer, blocks or fails if full
  - `ChannelRecv(endpoint, buf_ptr, buf_len)` -- copies message out, blocks if empty
  - `ChannelClose(endpoint)` -- destroys the endpoint
- Per-task table of owned endpoints (so the kernel can validate access and clean up on exit)
- A way to pass an endpoint to a child task at spawn time (e.g. Spawn takes an extra argument specifying which endpoint the child inherits; child receives it in a register or at a known memory address)
- Task `Blocked` state: scheduler skips blocked tasks, `ChannelSend` wakes blocked receivers, `ChannelRecv` wakes blocked senders

**Design constraints:**
- Fixed max message size (e.g. 4 KiB) to avoid dynamic kernel allocations per message
- Start with blocking send/recv only; non-blocking variants later

**Files to modify/create:**
- `kernel_api_types/src/lib.rs` -- new syscall numbers and channel types
- `kernel/src/ipc/` (new module) -- channel implementation, endpoint table
- `kernel/src/syscall_handlers.rs` -- channel syscall handlers
- `kernel/src/raw_syscall_handler.rs` -- register channel syscalls
- `kernel/src/task/task.rs` -- per-task endpoint table, Blocked state integration
- `ulib/src/lib.rs` -- channel wrappers

**Tests:**
- Init spawns a child, sends it a message, child receives and echoes it back. Init verifies the reply.

---

## Phase 3: Restrict Graphics Syscalls

**Status:** Done
**Blocked by:** Phase 2 (need IPC so non-display tasks can still request drawing)

**Goal:** Only a designated "display server" task can call `PresentDisplay` / `GetBoundingBox`.

**What to build:**
- A global `AtomicU64` storing the task ID of the current display owner (init by default)
- `PresentDisplay`, `GetBoundingBox` check the caller's task ID against the display owner; return error if mismatch
- A mechanism for the display owner to transfer ownership to a child (e.g. a `TransferDisplay` syscall, or a flag on `Spawn`)
- Alternatively, a per-task capability flag set at spawn time

**Files to modify/create:**
- `kernel/src/syscall_handlers.rs` -- add ownership check to graphics syscalls, add transfer mechanism
- `kernel_api_types/src/lib.rs` -- new error codes or syscall for transfer

**Tests:**
- A non-display task calls `PresentDisplay` and gets an error back.
- Display owner transfers ownership to a child; child can draw, parent can no longer draw.

---

## Phase 4: Display Server Task

**Status:** Not started
**Blocked by:** Phase 2 (IPC), Phase 3 (display ownership)

**Goal:** A dedicated user task that owns the framebuffer and renders on behalf of clients via IPC.

**What to build:**
- A new user binary: `display_server/` -- receives draw commands over IPC, renders via graphics syscalls
- A wire protocol for draw commands (serialized into message bytes):
  - `FillRect { x, y, w, h, color }`
  - `DrawPixels { data: [PixelData] }`
  - `GetBounds` -- reply: `{ w, h }`
- A client library (module in `ulib/` or separate crate) that wraps the protocol and implements `embedded_graphics::DrawTarget`, so existing drawing code works transparently
- Init spawns the display server (passing it display ownership + recv endpoint), then spawns client tasks with send endpoints

**Files to modify/create:**
- `display_server/` (new crate) -- display server binary
- `ulib/src/display_client.rs` (new) -- client-side DrawTarget wrapping IPC
- `kernel_api_types/src/lib.rs` -- display protocol message types (shared between server and clients)

**Tests:**
- Init spawns display server + one client. Client sends "fill red rectangle" command. Rectangle appears on screen.

---

## Phase 5: Visual Demo -- Multiple Cooperating Tasks

**Status:** Not started
**Blocked by:** Phase 4 (display server)

**Goal:** Multiple client tasks each drawing to different screen regions, coordinated by the display server.

**What to build:**
- Extend the display server protocol:
  - `RegisterWindow { x, y, w, h }` -- reply: `window_id` -- client requests a screen region
  - All subsequent draw commands from that client are clipped/offset to its window
  - `UnregisterWindow { window_id }`
- Spawn 2-3 demo client tasks, each claiming a different window:
  - Task A: animated color cycling
  - Task B: bouncing shape
  - Task C: text counter
- The display server clips each client's draws to its window bounds

**Files to modify/create:**
- `display_server/` -- extend protocol with windowing
- `init_task/` -- demo client tasks (could be separate binaries or one binary with different entry behavior)

**Tests:**
- Three independent animations running simultaneously in different screen regions, all through the display server.

---

## Dependency Graph

```
Phase 1: Spawn
   |
   v
Phase 2: IPC (message passing)
   |
   +------------------+
   v                  v
Phase 3: Restrict    Phase 4: Display
  graphics            server
   |                  |
   +--------+---------+
            v
     Phase 5: Visual demo
```

Phases 3 and 4 can be developed in parallel once Phase 2 is complete.

---

## Future Work (beyond this roadmap)

These are not needed for the visual demo but are natural next steps:

- **Shared memory IPC** -- for large data transfers (e.g. per-window framebuffers). Add `ShmCreate` / `ShmMap` syscalls that map the same physical frames into two tasks at the same virtual address.
- **Full compositor** -- double-buffered per-window framebuffers, alpha blending, z-ordering, window decorations.
- **Multiple ELF binaries** -- currently there is only one Limine module. For distinct binaries, either embed child ELFs as data in the init binary, or load multiple Limine modules.
- **Filesystem** -- loading programs from a filesystem instead of embedding them.
- **Non-blocking / async IPC** -- `poll`-style multiplexing across multiple channels.
