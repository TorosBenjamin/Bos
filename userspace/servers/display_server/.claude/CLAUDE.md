# Display Server

Before modifying this component, read `docs/display_server.md` for the full architecture (compositor pipeline, window lifecycle, IPC protocol, tiling layout, input handling).

## Quick Reference

- **Tests**: `cargo test -p display_server` (29 tests)
- **IPC protocol**: `[type: u8][request struct (unaligned)][optional reply_ep: u64]` — use `ptr::read_unaligned` for all struct reads at offset 1
- **Window types**: Toplevel (tiled or floating) and Panel (edge-anchored with exclusive zone)
- **Shared buffer lifecycle**: Create on window create/reconfigure → client writes pixels → DS reads during composite → destroy on close or after client acks reconfigure
- **Reconfigure race**: Old buffer IDs are queued in `pending_old_buf_ids`, NOT destroyed immediately. Destroyed only when the client sends UpdateWindow (ack). See `known_issues.md` "Premature Shared Buffer Destruction".
- **Dirty rects**: Each window tracks `pending_dirty` independently. Never merge into a single bounding box. Never call `display.present()` inside the IPC drain loop.

## Key Files

- `src/compositor/event_loop.rs` — main run loop (60 FPS)
- `src/compositor/handlers.rs` — IPC message dispatch
- `src/compositor/render.rs` — compositing pipeline
- `src/compositor/layout.rs` — dwindle tiling algorithm
- `src/window.rs` — Window struct, shared buffer lifecycle
