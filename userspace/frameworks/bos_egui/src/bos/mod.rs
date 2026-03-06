use linked_list_allocator::LockedHeap;
use kernel_api_types::{MMAP_WRITE, SVC_ERR_NOT_FOUND};
use ulib::window::{Window, WindowEvent};
use crate::App;

static mut CHILD_REQUEST: Option<(u32, u32)> = None;

pub(crate) fn request_open_child(w: u32, h: u32) {
    unsafe { core::ptr::addr_of_mut!(CHILD_REQUEST).write(Some((w, h))); }
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

pub fn run<A: App>(name: &str, mut app: A) -> ! {
    // Heap: 8 MB is plenty for the stub (no font atlas, no tessellation)
    let heap_size: usize = 8 * 1024 * 1024;
    let heap_ptr = ulib::sys_mmap(heap_size as u64, MMAP_WRITE);
    unsafe { ALLOCATOR.lock().init(heap_ptr, heap_size) }

    // Wait for display service
    let display_ep = loop {
        let ep = ulib::sys_lookup_service(b"display");
        if ep != SVC_ERR_NOT_FOUND { break ep; }
        ulib::sys_yield();
    };

    // Create toplevel window
    let mut window = loop {
        match Window::new(display_ep, name) {
            Some(w) => break w,
            None => ulib::sys_yield(),
        }
    };
    let main_id = window.window_id();

    // `frame_presented`: the compositor has finished presenting our last frame — safe to render.
    // `needs_redraw`: some input changed state — we have something new to draw.
    // Render only when both are true so we pace exactly one frame per compositor cycle.
    let mut frame_presented = true;
    let mut needs_redraw = true; // draw the initial frame immediately
    let mut cursor_x: f32 = (window.width() / 2) as f32;
    let mut cursor_y: f32 = (window.height() / 2) as f32;
    let mut click: Option<(f32, f32)> = None;

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
                WindowEvent::MouseMove { x, y } => {
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
                _ => {}
            }
        }

        if frame_presented && needs_redraw {
            frame_presented = false;
            needs_redraw = false;

            let w = window.width();
            let h = window.height();
            let info = *window.display_info();
            let pixels = window.pixels_mut();

            let ctx = stub_egui::Context::new(pixels, w, h, info, cursor_x, cursor_y, click.take());
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
        if let Some((cw, ch)) = child_req {
            if child.is_none() {
                if let Some(cwin) = Window::new_floating(display_ep, name, main_id, cw, ch, 0) {
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
                    WindowEvent::MouseMove { x, y } => {
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
                    cs.cursor_x, cs.cursor_y, cs.click.take(),
                );
                app.child_update(&child_ctx);

                cs.window.mark_dirty_all();
                cs.window.present();
            }
        }

        ulib::sys_yield();
    }
}
