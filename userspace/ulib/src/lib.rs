#![no_std]

pub mod display;
pub mod window;
pub mod test_framework;

use core::arch::asm;
use kernel_api_types::{SysCallNumber, SVC_ERR_NOT_FOUND, SVC_OK};
use kernel_api_types::graphics::{DisplayInfo, GraphicsResult, Rect};

pub fn syscall(inputs_and_ouputs: &mut [u64; 7]) {
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

pub fn sys_get_bounding_box(out_rect: &mut Rect) -> GraphicsResult {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::GetBoundingBox as u64;
    args[1] = out_rect as *const Rect as u64;

    syscall(&mut args);

    let ret = args[6];
    GraphicsResult::from_u64(ret)
}

pub fn sys_get_display_info() -> DisplayInfo {
    let mut info = DisplayInfo {
        width: 0,
        height: 0,
        red_mask_size: 0,
        red_mask_shift: 0,
        green_mask_size: 0,
        green_mask_shift: 0,
        blue_mask_size: 0,
        blue_mask_shift: 0,
    };
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::GetDisplayInfo as u64;
    args[1] = &mut info as *mut DisplayInfo as u64;

    syscall(&mut args);

    info
}

pub fn sys_read_mouse() -> Option<kernel_api_types::MouseEvent> {
    let mut event = kernel_api_types::MouseEvent::EMPTY;
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::ReadMouse as u64;
    args[1] = &mut event as *mut kernel_api_types::MouseEvent as u64;

    syscall(&mut args);

    if args[6] == 0 { Some(event) } else { None }
}

pub fn sys_read_key() -> kernel_api_types::KeyEvent {
    let mut event = kernel_api_types::KeyEvent::EMPTY;
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::ReadKey as u64;
    args[1] = &mut event as *mut kernel_api_types::KeyEvent as u64;

    syscall(&mut args);

    event
}

pub fn sys_yield() {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::Yield as u64;
    syscall(&mut args);
}

pub fn sys_mmap(size: u64, flags: u64) -> *mut u8 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::Mmap as u64;
    args[1] = size;
    args[2] = flags;
    syscall(&mut args);
    args[6] as *mut u8
}

pub fn sys_munmap(addr: *mut u8, size: u64) -> u64 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::Munmap as u64;
    args[1] = addr as u64;
    args[2] = size;
    syscall(&mut args);
    args[6]
}

pub fn sys_spawn(elf_bytes: &[u8], child_arg: u64) -> u64 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::Spawn as u64;
    args[1] = elf_bytes.as_ptr() as u64;
    args[2] = elf_bytes.len() as u64;
    args[3] = child_arg;
    syscall(&mut args);
    args[6]
}

pub fn sys_channel_create(capacity: u64) -> (u64, u64) {
    let mut send_ep: u64 = 0;
    let mut recv_ep: u64 = 0;
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::ChannelCreate as u64;
    args[1] = &mut send_ep as *mut u64 as u64;
    args[2] = &mut recv_ep as *mut u64 as u64;
    args[3] = capacity;
    syscall(&mut args);
    (send_ep, recv_ep)
}

pub fn sys_channel_send(endpoint_id: u64, data: &[u8]) -> u64 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::ChannelSend as u64;
    args[1] = endpoint_id;
    args[2] = data.as_ptr() as u64;
    args[3] = data.len() as u64;
    syscall(&mut args);
    args[6]
}

pub fn sys_channel_recv(endpoint_id: u64, buf: &mut [u8]) -> (u64, u64) {
    let mut bytes_read: u64 = 0;
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::ChannelRecv as u64;
    args[1] = endpoint_id;
    args[2] = buf.as_mut_ptr() as u64;
    args[3] = buf.len() as u64;
    args[4] = &mut bytes_read as *mut u64 as u64;
    syscall(&mut args);
    (args[6], bytes_read)
}

pub fn sys_channel_close(endpoint_id: u64) -> u64 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::ChannelClose as u64;
    args[1] = endpoint_id;
    syscall(&mut args);
    args[6]
}

pub fn sys_transfer_display(new_owner_task_id: u64) -> u64 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::TransferDisplay as u64;
    args[1] = new_owner_task_id;
    syscall(&mut args);
    args[6]
}

/// Emit a debug value to the kernel serial console.
/// `tag` is a u64 label printed alongside `value`.
pub fn sys_debug_log(value: u64, tag: u64) {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::DebugLog as u64;
    args[1] = value;
    args[2] = tag;
    syscall(&mut args);
}

pub fn sys_get_module(name: &str, buf: *mut u8, buf_cap: u64) -> u64 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::GetModule as u64;
    args[1] = name.as_ptr() as u64;
    args[2] = name.len() as u64;
    args[3] = buf as u64;
    args[4] = buf_cap;
    syscall(&mut args);
    args[6]
}

/// Register a send endpoint under a human-readable service name.
/// Returns `SVC_OK` on success or a `SVC_ERR_*` code on failure.
pub fn sys_register_service(name: &[u8], send_ep: u64) -> u64 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::RegisterService as u64;
    args[1] = name.as_ptr() as u64;
    args[2] = name.len() as u64;
    args[3] = send_ep;
    syscall(&mut args);
    args[6]
}

/// Look up a service by name.
/// Returns the send endpoint ID on success, or `SVC_ERR_NOT_FOUND` if not yet registered.
pub fn sys_lookup_service(name: &[u8]) -> u64 {
    let mut ep_out: u64 = 0;
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::LookupService as u64;
    args[1] = name.as_ptr() as u64;
    args[2] = name.len() as u64;
    args[3] = &mut ep_out as *mut u64 as u64;
    syscall(&mut args);
    if args[6] == SVC_OK {
        ep_out
    } else {
        SVC_ERR_NOT_FOUND
    }
}

pub fn sys_shutdown(exit_code: u64) -> ! {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::Shutdown as u64;
    args[1] = exit_code;
    syscall(&mut args);
    loop {}
}

pub fn default_panic(_info: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
