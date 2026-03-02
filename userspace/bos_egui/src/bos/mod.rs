use linked_list_allocator::LockedHeap;
use kernel_api_types::{MMAP_WRITE, SVC_ERR_NOT_FOUND};
use ulib::window::{Window, WindowEvent};
use crate::App;

pub mod stub_egui;
mod pixel_draw;

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

#[alloc_error_handler]
fn oom(_: core::alloc::Layout) -> ! {
    loop { core::hint::spin_loop(); }
}

pub fn run<A: App>(name: &str, mut app: A) -> ! {
    let _ = name;

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
        match Window::new(display_ep) {
            Some(w) => break w,
            None => ulib::sys_yield(),
        }
    };

    let mut frame_presented = true;
    let mut cursor_x: f32 = (window.width() / 2) as f32;
    let mut cursor_y: f32 = (window.height() / 2) as f32;
    let mut click: Option<(f32, f32)> = None;

    loop {
        // Drain window events
        while let Some(event) = window.poll_event() {
            match event {
                WindowEvent::FramePresented => frame_presented = true,
                WindowEvent::Configure { shared_buf_id, width: nw, height: nh } => {
                    window.apply_configure(shared_buf_id, nw, nh);
                    frame_presented = true;
                }
                WindowEvent::MouseButtonPress { x, y, .. } => {
                    cursor_x = x as f32;
                    cursor_y = y as f32;
                    click = Some((cursor_x, cursor_y));
                }
                WindowEvent::MouseButtonRelease { x, y, .. } => {
                    cursor_x = x as f32;
                    cursor_y = y as f32;
                }
                _ => {}
            }
        }

        if frame_presented {
            frame_presented = false;

            let w = window.width();
            let h = window.height();
            let info = *window.display_info();
            let pixels = window.pixels_mut();

            // Build the per-frame stub context and call the app
            let ctx = stub_egui::Context::new(pixels, w, h, info, cursor_x, cursor_y, click.take());
            app.update(&ctx);
            // Widgets have already been drawn into `pixels` via the context.

            window.mark_dirty_all();
            window.present();
        }

        ulib::sys_yield();
    }
}
