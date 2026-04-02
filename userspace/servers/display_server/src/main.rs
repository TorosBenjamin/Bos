//! # Display Server
//!
//! Tiling window compositor for Bos OS. Manages windows, composites their
//! pixel buffers onto the framebuffer, and routes input events.
//!
//! See `docs/display_server.md` for the full architecture documentation.

#![no_std]
#![no_main]

mod compositor;
mod compositor_config;
mod cursor;
mod window;

use compositor::Compositor;
use compositor_config::DisplayConfig;
use kernel_api_types::SVC_ERR_NOT_FOUND;

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point(_arg: u64) -> ! {
    let (send_fd, recv_fd) = ulib::handle::channel(16).unwrap();
    ulib::handle::register_service(b"display", send_fd);
    ulib::log::write(ulib::log::LogLevel::Info, "display", "started");

    let config = load_config();
    let mut compositor = Compositor::new(recv_fd, config);
    compositor.run()
}

/// Try to load `/bos_ds.conf` from the `fatfs` service.
/// Polls for up to ~200 ms (200 yields), then falls back to defaults on any failure.
fn load_config() -> DisplayConfig {
    // Wait up to 200 yields for the fatfs service to come up.
    let fs_fd = {
        let mut ep = SVC_ERR_NOT_FOUND;
        for _ in 0..200u32 {
            ep = ulib::sys_lookup_service(b"fatfs");
            if ep != SVC_ERR_NOT_FOUND {
                break;
            }
            ulib::sys_yield();
        }
        if ep == SVC_ERR_NOT_FOUND {
            return DisplayConfig::default();
        }
        match ulib::handle::handle_from_channel(ep, 1) {
            Some(fd) => fd,
            None => return DisplayConfig::default(),
        }
    };

    let (buf_id, file_size) = match ulib::fs::fs_map_file(fs_fd, "/bos_ds.conf") {
        Some(v) => v,
        None => {
            ulib::handle::close(fs_fd);
            return DisplayConfig::default();
        }
    };

    ulib::handle::close(fs_fd);

    let ptr = ulib::sys_map_shared_buf(buf_id);
    if ptr.is_null() {
        ulib::sys_destroy_shared_buf(buf_id);
        return DisplayConfig::default();
    }

    let bytes = unsafe { core::slice::from_raw_parts(ptr, file_size as usize) };
    let cfg = DisplayConfig::parse(bytes);

    // Unmap our view and free the shared buffer
    ulib::sys_munmap(ptr, file_size);
    ulib::sys_destroy_shared_buf(buf_id);

    cfg
}

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    ulib::default_panic(info)
}
