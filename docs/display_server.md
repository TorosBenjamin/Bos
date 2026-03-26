# Display Server Architecture

The display server is a tiling Wayland-inspired compositor that runs entirely in
userspace. It owns the framebuffer, manages windows, composites their pixel
buffers, and routes keyboard/mouse input.

**Source:** `userspace/servers/display_server/`

## Module Layout

```
src/
  main.rs                 Entry point, service registration, config loading
  window.rs               Window struct, shared buffer lifecycle, reconfigure
  cursor.rs               14x20 cursor sprite data
  compositor_config.rs    /bos_ds.conf parser (TOML subset)
  compositor/
    mod.rs                Compositor struct, initialization
    event_loop.rs         Main run loop (60 FPS, event-driven)
    handlers.rs           IPC message dispatch (Create/Update/Close/...)
    render.rs             Compositing pipeline, blitting, present
    layout.rs             Dwindle tiling algorithm
    focus.rs              Z-order, hit testing, keyboard shortcuts
    drag.rs               Window move/resize state machines
```

## Startup

1. Create an IPC channel and register as the `"display"` service.
2. Load `/bos_ds.conf` from the filesystem (polls for fs_server readiness).
3. Pre-render a gradient background into a RAM buffer.
4. Enter the main run loop.

## Window Model

There are two window types:

**Toplevel windows** are the normal application windows. They can be either
*tiled* (DS controls size/position via the dwindle layout) or *floating* (client
requests a size, DS centers it). Toplevels with a `parent_id` are always
floating (dialog model).

**Panel windows** anchor to a screen edge (Top/Bottom/Left/Right) and reserve
an exclusive zone that toplevels avoid. Panels are always above all toplevels
in z-order. Example: a taskbar.

Whether a toplevel is floating or tiled is determined by (in priority order):
1. Window rules in `/bos_ds.conf`
2. Has a parent window (always float)
3. `WINDOW_FLAG_FLOATING` flag
4. Default: tiled

## Window Lifecycle

### Creation

Client sends `CreateWindowRequest` (or `CreatePanelRequest`) with an
`event_send_ep` for receiving events from the DS.

DS allocates a `Window` struct with a shared physical buffer
(`sys_create_shared_buf`), computes the window's position (tiling layout or
centered for floating), and replies with `CreateWindowResponse` containing the
`window_id`, `shared_buf_id`, and initial dimensions.

Both client and DS map the same physical pages — the client writes pixels, the
DS reads them during compositing. No pixel data is ever copied over IPC.

### Rendering

The client draws into the shared buffer, then sends `UpdateWindowRequest` with
a dirty rectangle. The DS does NOT respond — it simply records the dirty rect
and composites it during the next frame's flush.

### Reconfigure (resize)

When the tiling layout changes (e.g. a new window is added), the DS calls
`window.reconfigure(new_x, new_y, new_w, new_h)`. If dimensions changed:

1. Allocate a new shared buffer for the new size.
2. Queue the old buffer ID in `pending_old_buf_ids` (do NOT destroy it yet).
3. Unmap the DS's view of the old buffer.
4. Send a `ConfigureEvent` to the client with the new `shared_buf_id` and size.

The client receives `ConfigureEvent`, maps the new buffer, unmaps the old one,
re-renders, and sends `UpdateWindowRequest`. When the DS processes that
UpdateWindow, it destroys all queued old buffer IDs. This deferred destruction
prevents a race where the client is still writing to the old buffer.

### Close

1. DS sends `Close` event to the client.
2. Client can do cleanup, then exit (or acknowledge).
3. DS polls closing windows each frame; after ~333ms timeout it force-cleans.
4. `complete_cleanup` unmaps buffers, destroys shared buffers, closes channels.

## IPC Protocol

All messages are sent over the DS's service channel. Format:

```
[type: u8][request struct (unaligned)][optional reply_ep: u64]
```

| Type | Message | Direction | Response |
|------|---------|-----------|----------|
| 0 | CreateWindow | client -> DS | CreateWindowResponse on reply_ep |
| 1 | UpdateWindow | client -> DS | (none) |
| 2 | CloseWindow | client -> DS | Close event on event channel |
| 7 | CreatePanel | client -> DS | CreateWindowResponse on reply_ep |
| 8 | HideWindow | client -> DS | (none) |
| 9 | ShowWindow | client -> DS | (none) |

Events sent from DS to client over the persistent event channel:

| Event | When |
|-------|------|
| KeyPress | Keyboard input (focused window only) |
| FocusGained/Lost | Focus change |
| Configure | DS resized the window (new buffer) |
| FramePresented | Frame composited (pacing signal) |
| MouseMove | Cursor moved within window |
| MouseButtonPress/Release | Mouse click within window |
| Close | Window is being closed |

## Compositing Pipeline

Each frame (capped at 60 FPS):

1. **Poll closing windows** — detect exited clients, timeout force-close.
2. **Drain IPC messages** — non-blocking, up to 64 per frame.
3. **Drain mouse events** — accumulate into one cursor delta.
4. **Update drag state** — reposition/resize windows being dragged.
5. **Process keyboard** — check shortcuts first, then forward to focused window.
6. **Flush** — the actual compositing step:
   - If full redraw: blit background, composite all windows in z-order, cursor, present.
   - Otherwise: for each window with a dirty rect, blit background into that rect,
     composite overlapping windows clipped to the rect. Draw cursor once. Present
     all dirty rects to VRAM in a single pass.

**Per-window dirty rects** are critical — without them, two small updates at
opposite corners of the screen would merge into a full-screen bounding box.
Each window tracks its own `pending_dirty` independently.

## Tiling Layout (Dwindle)

The dwindle algorithm splits available space in a golden-spiral pattern:

```
Direction cycle: Horizontal -> Vertical -> HorizontalReversed -> VerticalReversed -> ...
```

Window 0 takes a ratio of the available space on one side; remaining windows
recursively split the other side. The split ratios are stored per-window and
can be adjusted via Super+right-drag on a split edge.

Available area is the screen minus the exclusive zones of all panels.

## Focus and Input

- **Click-to-focus**: clicking a window raises it and gives it keyboard focus.
- **Keyboard shortcuts** (configurable in `/bos_ds.conf`):
  - `Super+Q` — close focused window
  - `Alt+Tab` / `Alt+Shift+Tab` — cycle focus
  - `Super+Arrow` — directional focus
  - `Super+Space` — toggle launcher
- **Super+left-drag** — move floating windows or swap tiled windows.
- **Super+right-drag** — resize (floating: edge resize, tiled: adjust split ratio).

## Configuration

`/bos_ds.conf` is a TOML-subset file loaded at startup:

```toml
[general]
outer_gap = 8
inner_gap = 8
border_size = 2
inactive_opacity = 80

[colors]
border_focused = #8aadf4
border_unfocused = #363a4f
bg_top = #1e3a5f
bg_bottom = #0a0a0f

[window_rules]
launcher = float

[shortcuts]
close_window = super+q
toggle_launcher = super+space

[protected_windows]
display_server
```
