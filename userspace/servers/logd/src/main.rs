#![no_std]
#![no_main]

mod server;

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    ulib::default_panic(info)
}

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point(_arg: u64) -> ! {
    let (send_fd, recv_fd) = ulib::handle::channel(64).expect("logd: channel create failed");
    ulib::handle::register_service(b"logd", send_fd);
    server::run(recv_fd)
}
