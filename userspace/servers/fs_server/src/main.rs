//! # Filesystem Server
//!
//! FAT32 filesystem server for Bos OS. Reads and writes files on an IDE disk
//! via IPC to the IDE driver, and serves file requests to userspace clients.
//!
//! See `docs/fs_server.md` for the full architecture documentation.

#![no_std]
#![no_main]

mod fat32;
mod server;

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point(_arg: u64) -> ! {
    let (send_fd, recv_fd) = ulib::handle::channel(16).unwrap();
    ulib::handle::register_service(b"fatfs", send_fd);
    server::run(recv_fd)
}

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    ulib::default_panic(info)
}
