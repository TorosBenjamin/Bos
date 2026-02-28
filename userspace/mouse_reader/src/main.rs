#![no_std]
#![no_main]

use kernel_api_types::window::{MouseInputMessage, WindowMessageType};
use kernel_api_types::SVC_ERR_NOT_FOUND;

#[panic_handler]
fn rust_panic(_info: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point(_arg: u64) -> ! {
    // Wait until the display server has registered the "display" service.
    let display_ep = loop {
        let ep = ulib::sys_lookup_service(b"display");
        if ep != SVC_ERR_NOT_FOUND {
            break ep;
        }
        ulib::sys_yield();
    };

    // Message layout: [type: u8][MouseInputMessage: 5 bytes] = 6 bytes total.
    // MouseInputMessage starts at offset 1 (display server uses read_unaligned).
    const MSG_SIZE: usize = 1 + core::mem::size_of::<MouseInputMessage>();
    let mut buf = [0u8; MSG_SIZE];
    buf[0] = WindowMessageType::MouseInput as u8;

    loop {
        let Some(event) = ulib::sys_read_mouse() else {
            ulib::sys_yield();
            continue;
        };

        let msg = MouseInputMessage {
            dx: event.dx,
            dy: event.dy,
            buttons: event.buttons,
        };

        unsafe {
            core::ptr::copy_nonoverlapping(
                &msg as *const MouseInputMessage as *const u8,
                buf.as_mut_ptr().add(1),
                core::mem::size_of::<MouseInputMessage>(),
            );
        }

        ulib::sys_channel_send(display_ep, &buf);
    }
}
