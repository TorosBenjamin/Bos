#![no_std]
#![no_main]

mod compositor;
mod cursor;
mod window;

use compositor::Compositor;

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point(_arg: u64) -> ! {
    let (send_ep, recv_ep) = ulib::sys_channel_create(16);
    ulib::sys_register_service(b"display", send_ep);

    let mut compositor = Compositor::new(recv_ep);
    compositor.run()
}

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    ulib::default_panic(info)
}
