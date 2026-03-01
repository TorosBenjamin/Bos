#![no_std]
#![no_main]

mod fat32;
mod server;

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point(_arg: u64) -> ! {
    let (send_ep, recv_ep) = ulib::sys_channel_create(16);
    ulib::sys_register_service(b"fatfs", send_ep);
    server::run(recv_ep)
}

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    ulib::default_panic(info)
}
