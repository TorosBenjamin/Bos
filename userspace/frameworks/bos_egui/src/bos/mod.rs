extern crate alloc;
use linked_list_allocator::LockedHeap;
use kernel_api_types::{MMAP_WRITE, SVC_ERR_NOT_FOUND};
use ulib::window::{Window, WindowEvent, KeyEvent};
use crate::App;

static mut CHILD_REQUEST: Option<(u32, u32)> = None;
static mut REDRAW_REQUESTED: bool = false;
/// When non-zero, the run loop sleeps at most this many ms before the next frame.
static mut TIMED_REDRAW_MS: u32 = 0;

pub(crate) fn request_open_child(w: u32, h: u32) {
    unsafe { core::ptr::addr_of_mut!(CHILD_REQUEST).write(Some((w, h))); }
}

pub(crate) fn request_redraw_impl() {
    unsafe { core::ptr::addr_of_mut!(REDRAW_REQUESTED).write(true); }
}

pub(crate) fn request_timed_redraw_impl(ms: u32) {
    // Only set the timer — do NOT set REDRAW_REQUESTED.  The run loop will
    // sleep on handle::wait with this timeout and only force a redraw
    // when the timeout actually fires, avoiding a render-every-frame spin.
    unsafe {
        core::ptr::addr_of_mut!(TIMED_REDRAW_MS).write(ms);
    }
}

struct ChildState {
    window: Window,
    frame_presented: bool,
    needs_redraw: bool,
    cursor_x: f32,
    cursor_y: f32,
    click: Option<(f32, f32)>,
}

pub mod stub_egui;
mod pixel_draw;

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

#[alloc_error_handler]
fn oom(_: core::alloc::Layout) -> ! {
    loop { core::hint::spin_loop(); }
}

pub fn run<A: App>(name: &str, app: A) -> ! {
    run_with_heap(name, app, 8 * 1024 * 1024)
}

pub fn run_with_heap<A: App>(name: &str, mut app: A, heap_size: usize) -> ! {
    let rsp0: u64;
    unsafe { core::arch::asm!("mov {}, rsp", out(reg) rsp0); }
    ulib::sys_debug_log(heap_size as u64, 0xE001); // E001 = run_with_heap entry, also RSP next
    ulib::sys_debug_log(rsp0, 0xE00A);             // E00A = RSP at entry

    let heap_ptr = ulib::sys_mmap(heap_size as u64, MMAP_WRITE);
    ulib::sys_debug_log(heap_ptr as u64, 0xE002); // E002 = heap_ptr from sys_mmap
    unsafe { ALLOCATOR.lock().init(heap_ptr, heap_size) }
    ulib::sys_debug_log(0, 0xE003); // E003 = allocator initialized

    // Wait for display service
    let display_ep = loop {
        let ep = ulib::sys_lookup_service(b"display");
        if ep != SVC_ERR_NOT_FOUND { break ep; }
        ulib::sys_yield();
    };
    ulib::sys_debug_log(display_ep, 0xE004); // E004 = display_ep found

    // Wrap the raw endpoint as a handle fd for the new handle-based IPC API.
    let display_fd = ulib::handle::handle_from_channel(display_ep, 1)
        .expect("failed to wrap display endpoint as handle");
    let rsp5: u64;
    unsafe { core::arch::asm!("mov {}, rsp", out(reg) rsp5); }
    ulib::sys_debug_log(display_fd as u64, 0xE005); // E005 = display_fd
    ulib::sys_debug_log(rsp5, 0xE00B);              // E00B = RSP before Window::new loop

    // Create toplevel window
    let mut window = loop {
        let rsp_w: u64;
        unsafe { core::arch::asm!("mov {}, rsp", out(reg) rsp_w); }
        ulib::sys_debug_log(rsp_w, 0xE00C); // E00C = RSP at each Window::new attempt
        match Window::new(display_fd, name) {
            Some(w) => break w,
            None => ulib::sys_yield(),
        }
    };
    ulib::sys_debug_log(0, 0xE006); // E006 = window created
    let main_id = window.window_id();

    // `frame_presented`: the compositor has finished presenting our last frame — safe to render.
    // `needs_redraw`: some input changed state — we have something new to draw.
    // Render only when both are true so we pace exactly one frame per compositor cycle.
    let mut frame_presented = true;
    let mut needs_redraw = true; // draw the initial frame immediately
    let mut cursor_x: f32 = (window.width() / 2) as f32;
    let mut cursor_y: f32 = (window.height() / 2) as f32;
    let mut click: Option<(f32, f32)> = None;
    let mut keys: alloc::vec::Vec<KeyEvent> = alloc::vec::Vec::new();

    let mut child: Option<ChildState> = None;

    loop {
        // Drain all pending main-window events.
        while let Some(event) = window.poll_event() {
            match event {
                WindowEvent::FramePresented => frame_presented = true,
                WindowEvent::Configure { shared_buf_id, width: nw, height: nh } => {
                    window.apply_configure(shared_buf_id, nw, nh);
                    frame_presented = true;
                    needs_redraw = true;
                }
                WindowEvent::MouseMove { x, y, .. } => {
                    cursor_x = x as f32;
                    cursor_y = y as f32;
                    needs_redraw = true;
                }
                WindowEvent::MouseButtonPress { x, y, .. } => {
                    cursor_x = x as f32;
                    cursor_y = y as f32;
                    click = Some((cursor_x, cursor_y));
                    needs_redraw = true;
                    frame_presented = true;
                }
                WindowEvent::MouseButtonRelease { x, y, .. } => {
                    cursor_x = x as f32;
                    cursor_y = y as f32;
                    needs_redraw = true;
                    frame_presented = true;
                }
                WindowEvent::KeyPress(ev) => {
                    keys.push(ev);
                    needs_redraw = true;
                    frame_presented = true;
                }
                _ => {}
            }
        }

        // Let the app signal it needs another render (e.g. after a state transition).
        let redraw_req = unsafe {
            let ptr = core::ptr::addr_of_mut!(REDRAW_REQUESTED);
            let v = ptr.read();
            ptr.write(false);
            v
        };
        if redraw_req { needs_redraw = true; }

        if frame_presented && needs_redraw {
            frame_presented = false;
            needs_redraw = false;
            // Clear the timed-redraw timer so the app must re-request it
            // each frame (allows it to stop the timer by not calling it).
            unsafe { core::ptr::addr_of_mut!(TIMED_REDRAW_MS).write(0); }

            let w = window.width();
            let h = window.height();
            let info = *window.display_info();
            let pixels = window.pixels_mut();

            let ctx = stub_egui::Context::new(pixels, w, h, info, cursor_x, cursor_y, click.take(), core::mem::take(&mut keys), !app.skip_bg_clear());
            app.update(&ctx);

            window.mark_dirty_all();
            window.present();
        }

        // Open child window if the app requested it this frame.
        let child_req = unsafe {
            let ptr = core::ptr::addr_of_mut!(CHILD_REQUEST);
            let val = ptr.read();
            ptr.write(None);
            val
        };
        #[allow(clippy::collapsible_if)]
        if let Some((cw, ch)) = child_req {
            if child.is_none() {
                if let Some(cwin) = Window::new_floating(display_fd, name, main_id, cw, ch, 0) {
                    child = Some(ChildState {
                        window: cwin,
                        frame_presented: true,
                        needs_redraw: true,
                        cursor_x: 0.0,
                        cursor_y: 0.0,
                        click: None,
                    });
                }
            }
        }

        // Drive the child window if it exists.
        if let Some(ref mut cs) = child {
            while let Some(event) = cs.window.poll_event() {
                match event {
                    WindowEvent::FramePresented => cs.frame_presented = true,
                    WindowEvent::Configure { shared_buf_id, width: nw, height: nh } => {
                        cs.window.apply_configure(shared_buf_id, nw, nh);
                        cs.frame_presented = true;
                        cs.needs_redraw = true;
                    }
                    WindowEvent::MouseMove { x, y, .. } => {
                        cs.cursor_x = x as f32;
                        cs.cursor_y = y as f32;
                        cs.needs_redraw = true;
                    }
                    WindowEvent::MouseButtonPress { x, y, .. } => {
                        cs.cursor_x = x as f32;
                        cs.cursor_y = y as f32;
                        cs.click = Some((cs.cursor_x, cs.cursor_y));
                        cs.needs_redraw = true;
                        cs.frame_presented = true;
                    }
                    WindowEvent::MouseButtonRelease { x, y, .. } => {
                        cs.cursor_x = x as f32;
                        cs.cursor_y = y as f32;
                        cs.needs_redraw = true;
                        cs.frame_presented = true;
                    }
                    _ => {}
                }
            }

            if cs.frame_presented && cs.needs_redraw {
                cs.frame_presented = false;
                cs.needs_redraw = false;

                let cw = cs.window.width();
                let ch = cs.window.height();
                let info = *cs.window.display_info();
                let pixels = cs.window.pixels_mut();

                let child_ctx = stub_egui::Context::new(
                    pixels, cw, ch, info,
                    cs.cursor_x, cs.cursor_y, cs.click.take(), alloc::vec::Vec::new(), true,
                );
                app.child_update(&child_ctx);

                cs.window.mark_dirty_all();
                cs.window.present();
            }
        }

        // Sleep efficiently: wait for window events or a timed-redraw timeout.
        // Don't clear TIMED_REDRAW_MS here — it persists so we re-enter the
        // timed wait after processing events (e.g. FramePresented) that don't
        // trigger a render.  It is cleared at render time above.
        let timed_ms = unsafe { core::ptr::addr_of!(TIMED_REDRAW_MS).read() };
        if timed_ms > 0 {
            let fd = window.event_recv_fd();
            let fds = [fd];
            let ret = ulib::handle::wait(&fds, 0, timed_ms as u64);
            // ret == 1 means timeout (blink toggle); ret == 0 means a window
            // event arrived (FramePresented, key, mouse) — the poll loop above
            // will handle it on the next iteration without forcing a redraw.
            if ret == 1 {
                needs_redraw = true;
                // If FramePresented was lost (e.g. channel overflow during a long
                // blocking init), the loop would deadlock waiting for it forever.
                // On timeout, assume the compositor has presented and allow the
                // next render to proceed.
                frame_presented = true;
            }
        } else {
            ulib::sys_yield();
        }
    }
}
