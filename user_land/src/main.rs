#![no_std]
#![no_main]

mod display;
mod syscalls;

use core::{hint::black_box, sync::atomic::AtomicU8};
use core::arch::asm;
use crate::display::{draw_fun, Display};

#[panic_handler]
fn rust_panic(_info: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point() -> ! {
    let mut display = Display;
    draw_fun(&mut display);

    loop {
        core::hint::spin_loop();
    }
}

fn syscall(inputs_and_ouputs: &mut [u64; 7]) {
    unsafe {
        asm!("
            syscall
            ",
        inlateout("rdi") inputs_and_ouputs[0],
        inlateout("rsi") inputs_and_ouputs[1],
        inlateout("rdx") inputs_and_ouputs[2],
        inlateout("r10") inputs_and_ouputs[3],
        inlateout("r8") inputs_and_ouputs[4],
        inlateout("r9") inputs_and_ouputs[5],
        inlateout("rax") inputs_and_ouputs[6],
        );
    }
}