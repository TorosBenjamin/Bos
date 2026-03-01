#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU64, Ordering};

use kernel_api_types::{
    IPC_ERR_CHANNEL_FULL, IPC_ERR_INVALID_ENDPOINT, IPC_ERR_PEER_CLOSED,
    IPC_OK, MMAP_WRITE, SVC_ERR_NOT_FOUND, SVC_OK,
};
use ulib::fs;
use ulib::test_framework::TestRunner;

#[panic_handler]
fn rust_panic(_info: &core::panic::PanicInfo) -> ! {
    ulib::default_panic(_info)
}

// ---------------------------------------------------------------------------
// Memory tests
// ---------------------------------------------------------------------------

fn mmap_nonzero() -> bool {
    let ptr = ulib::sys_mmap(4096, MMAP_WRITE);
    if ptr.is_null() {
        return false;
    }
    let is_aligned = (ptr as u64) % 4096 == 0;
    ulib::sys_munmap(ptr, 4096);
    is_aligned
}

fn mmap_writable() -> bool {
    let ptr = ulib::sys_mmap(4096, MMAP_WRITE);
    if ptr.is_null() {
        return false;
    }
    unsafe {
        core::ptr::write(ptr as *mut u32, 0xDEAD_BEEF);
        let val = core::ptr::read(ptr as *const u32);
        ulib::sys_munmap(ptr, 4096);
        val == 0xDEAD_BEEF
    }
}

fn mmap_independent() -> bool {
    let a = ulib::sys_mmap(4096, MMAP_WRITE);
    let b = ulib::sys_mmap(4096, MMAP_WRITE);
    let different = !a.is_null() && !b.is_null() && a != b;
    ulib::sys_munmap(a, 4096);
    ulib::sys_munmap(b, 4096);
    different
}

fn munmap_ok() -> bool {
    let ptr = ulib::sys_mmap(4096, MMAP_WRITE);
    if ptr.is_null() {
        return false;
    }
    let ret = ulib::sys_munmap(ptr, 4096);
    ret == 0
}

// ---------------------------------------------------------------------------
// IPC tests
// ---------------------------------------------------------------------------

fn channel_create() -> bool {
    let (send_ep, recv_ep) = ulib::sys_channel_create(1);
    let ok = send_ep != 0 && recv_ep != 0 && send_ep != recv_ep;
    ulib::sys_channel_close(send_ep);
    ulib::sys_channel_close(recv_ep);
    ok
}

fn channel_loopback() -> bool {
    let (send_ep, recv_ep) = ulib::sys_channel_create(4);
    let data: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
    let send_result = ulib::sys_channel_send(send_ep, &data);
    if send_result != IPC_OK {
        ulib::sys_channel_close(send_ep);
        ulib::sys_channel_close(recv_ep);
        return false;
    }
    let mut buf = [0u8; 8];
    let (recv_result, bytes_read) = ulib::sys_channel_recv(recv_ep, &mut buf);
    ulib::sys_channel_close(send_ep);
    ulib::sys_channel_close(recv_ep);
    recv_result == IPC_OK && bytes_read == 8 && buf == data
}

fn channel_recv_size() -> bool {
    let (send_ep, recv_ep) = ulib::sys_channel_create(4);
    let data = [0xABu8; 5];
    ulib::sys_channel_send(send_ep, &data);
    let mut buf = [0u8; 64];
    let (result, bytes_read) = ulib::sys_channel_recv(recv_ep, &mut buf);
    ulib::sys_channel_close(send_ep);
    ulib::sys_channel_close(recv_ep);
    result == IPC_OK && bytes_read == 5
}

fn channel_full() -> bool {
    // Create a channel with capacity 4; the 5th send should return IPC_ERR_CHANNEL_FULL
    // via the timer-interrupt EINTR mechanism (blocks briefly, timer returns fallback rax).
    let (send_ep, recv_ep) = ulib::sys_channel_create(4);
    let data = [0u8; 1];
    for _ in 0..4 {
        if ulib::sys_channel_send(send_ep, &data) != IPC_OK {
            ulib::sys_channel_close(send_ep);
            ulib::sys_channel_close(recv_ep);
            return false;
        }
    }
    // 5th send: channel is full → EINTR returns IPC_ERR_CHANNEL_FULL
    let result = ulib::sys_channel_send(send_ep, &data);
    ulib::sys_channel_close(send_ep);
    ulib::sys_channel_close(recv_ep);
    result == IPC_ERR_CHANNEL_FULL
}

fn channel_close_peer() -> bool {
    let (send_ep, recv_ep) = ulib::sys_channel_create(4);
    // Close the receiving end (peer of send_ep)
    ulib::sys_channel_close(recv_ep);
    // Sending to a channel whose peer is closed should return PeerClosed or InvalidEndpoint
    let result = ulib::sys_channel_send(send_ep, &[1u8]);
    ulib::sys_channel_close(send_ep);
    result == IPC_ERR_PEER_CLOSED || result == IPC_ERR_INVALID_ENDPOINT
}

// ---------------------------------------------------------------------------
// Service registry tests
// ---------------------------------------------------------------------------

static UTEST_SERVICE_EP: AtomicU64 = AtomicU64::new(0);

fn service_register() -> bool {
    let (send_ep, _recv_ep) = ulib::sys_channel_create(1);
    let result = ulib::sys_register_service(b"utest_dummy", send_ep);
    if result == SVC_OK {
        UTEST_SERVICE_EP.store(send_ep, Ordering::Relaxed);
        true
    } else {
        ulib::sys_channel_close(send_ep);
        false
    }
}

fn service_lookup() -> bool {
    let expected = UTEST_SERVICE_EP.load(Ordering::Relaxed);
    let found = ulib::sys_lookup_service(b"utest_dummy");
    found == expected
}

fn service_lookup_missing() -> bool {
    ulib::sys_lookup_service(b"no_such_service") == SVC_ERR_NOT_FOUND
}

// ---------------------------------------------------------------------------
// Filesystem server tests
// ---------------------------------------------------------------------------

static FS_ENDPOINT: AtomicU64 = AtomicU64::new(0);

/// Poll the service registry until the "fatfs" service appears, then cache it.
fn wait_for_fs_service() {
    loop {
        let ep = ulib::sys_lookup_service(b"fatfs");
        if ep != SVC_ERR_NOT_FOUND {
            FS_ENDPOINT.store(ep, Ordering::Relaxed);
            return;
        }
        ulib::sys_yield();
    }
}

fn fs_service_registered() -> bool {
    ulib::sys_lookup_service(b"fatfs") != SVC_ERR_NOT_FOUND
}

fn fs_readdir_root() -> bool {
    let ep = FS_ENDPOINT.load(Ordering::Relaxed);
    match fs::fs_readdir(ep, "/") {
        Some(resp) => resp.count > 0,
        None => false,
    }
}

fn fs_stat_existing_file() -> bool {
    let ep = FS_ENDPOINT.load(Ordering::Relaxed);
    match fs::fs_stat(ep, "CUBE1.ELF") {
        Some(resp) => resp.is_dir == 0 && resp.size > 0,
        None => false,
    }
}

fn fs_map_file_elf_magic() -> bool {
    let ep = FS_ENDPOINT.load(Ordering::Relaxed);
    let (buf_id, file_size) = match fs::fs_map_file(ep, "CUBE1.ELF") {
        Some(v) => v,
        None => return false,
    };
    if file_size < 4 {
        ulib::sys_destroy_shared_buf(buf_id);
        return false;
    }
    let ptr = ulib::sys_map_shared_buf(buf_id);
    let magic_ok = if ptr.is_null() {
        false
    } else {
        unsafe {
            ptr.read() == 0x7F
                && ptr.add(1).read() == b'E'
                && ptr.add(2).read() == b'L'
                && ptr.add(3).read() == b'F'
        }
    };
    ulib::sys_destroy_shared_buf(buf_id);
    magic_ok
}

fn fs_map_missing_returns_none() -> bool {
    let ep = FS_ENDPOINT.load(Ordering::Relaxed);
    fs::fs_map_file(ep, "NOSUCHFILE.BIN").is_none()
}

fn fs_write_read_roundtrip() -> bool {
    let ep = FS_ENDPOINT.load(Ordering::Relaxed);

    // Create a shared buffer and fill it with known data
    let content = b"utest_write_roundtrip";
    let (buf_id, ptr) = ulib::sys_create_shared_buf(content.len() as u64);
    if ptr.is_null() || buf_id == u64::MAX {
        return false;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(content.as_ptr(), ptr, content.len());
    }

    // Write to disk
    let result = fs::fs_write_file(ep, "UTEST.TXT", buf_id, content.len() as u64);
    ulib::sys_destroy_shared_buf(buf_id);
    if result != kernel_api_types::fs::FsResult::Ok {
        return false;
    }

    // Read back and verify
    let (read_id, read_size) = match fs::fs_map_file(ep, "UTEST.TXT") {
        Some(v) => v,
        None => return false,
    };
    if read_size != content.len() as u64 {
        ulib::sys_destroy_shared_buf(read_id);
        return false;
    }
    let read_ptr = ulib::sys_map_shared_buf(read_id);
    let ok = if read_ptr.is_null() {
        false
    } else {
        unsafe {
            let read_slice = core::slice::from_raw_parts(read_ptr, content.len());
            read_slice == content
        }
    };
    ulib::sys_destroy_shared_buf(read_id);
    ok
}

// ---------------------------------------------------------------------------
// Display server tests
// ---------------------------------------------------------------------------

static DS_ENDPOINT: AtomicU64 = AtomicU64::new(0);

/// Poll the service registry until the "display" service appears, then cache its endpoint.
fn wait_for_display_service() {
    loop {
        let ep = ulib::sys_lookup_service(b"display");
        if ep != SVC_ERR_NOT_FOUND {
            DS_ENDPOINT.store(ep, Ordering::Relaxed);
            return;
        }
        ulib::sys_yield();
    }
}

fn display_info() -> bool {
    let info = ulib::sys_get_display_info();
    info.width > 0 && info.height > 0
}

fn display_registered() -> bool {
    ulib::sys_lookup_service(b"display") != SVC_ERR_NOT_FOUND
}

fn create_window_ok() -> bool {
    let ds_ep = DS_ENDPOINT.load(Ordering::Relaxed);
    ulib::window::Window::new(ds_ep, 100, 100, 0, 0).is_some()
}

fn create_window_bad_dims() -> bool {
    let ds_ep = DS_ENDPOINT.load(Ordering::Relaxed);
    // Width=0 should be rejected by display server as ErrorInvalidDimensions
    ulib::window::Window::new(ds_ep, 0, 100, 0, 0).is_none()
}

fn update_window() -> bool {
    use embedded_graphics::{
        draw_target::DrawTarget,
        geometry::{Point, Size},
        pixelcolor::{Rgb888, RgbColor},
        primitives::Rectangle,
    };

    let ds_ep = DS_ENDPOINT.load(Ordering::Relaxed);
    let mut window = match ulib::window::Window::new(ds_ep, 50, 50, 200, 200) {
        Some(w) => w,
        None => return false,
    };

    let red = Rgb888::RED;
    let area = Rectangle::new(Point::new(0, 0), Size::new(50, 50));
    let _ = window.fill_solid(&area, red);
    window.present();
    true
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point(_arg: u64) -> ! {
    let mut runner = TestRunner::new();

    // Memory tests
    runner.run(mmap_nonzero);
    runner.run(mmap_writable);
    runner.run(mmap_independent);
    runner.run(munmap_ok);

    // IPC tests
    runner.run(channel_create);
    runner.run(channel_loopback);
    runner.run(channel_recv_size);
    runner.run(channel_full);
    runner.run(channel_close_peer);

    // Service registry tests
    runner.run(service_register);
    runner.run(service_lookup);
    runner.run(service_lookup_missing);

    // Wait for display server before running display tests
    wait_for_display_service();

    // Display server tests
    runner.run(display_info);
    runner.run(display_registered);
    runner.run(create_window_ok);
    runner.run(create_window_bad_dims);
    runner.run(update_window);

    // Wait for filesystem server before running fs tests
    wait_for_fs_service();

    // Filesystem server tests
    runner.run(fs_service_registered);
    runner.run(fs_readdir_root);
    runner.run(fs_stat_existing_file);
    runner.run(fs_map_file_elf_magic);
    runner.run(fs_map_missing_returns_none);
    runner.run(fs_write_read_roundtrip);

    runner.finish()
}
