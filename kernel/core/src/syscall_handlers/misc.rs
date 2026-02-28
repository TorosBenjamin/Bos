use crate::limine_requests::MODULE_REQUEST;

/// Syscall: write to the isa-debug-exit port and halt (exits QEMU).
///
/// Arguments: exit_code — written directly to port 0xf4.
/// QEMU exits with code `(exit_code << 1) | 1`.
/// Convention: 0x10 = all tests passed (exit 33), 0x11 = any failure (exit 35).
pub fn sys_shutdown(exit_code: u64, _: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    unsafe { x86::io::outb(0xf4, exit_code as u8) }
    loop {}
}
use crate::memory::cpu_local_data::get_local;
use crate::task::task::TaskState;
use core::sync::atomic::Ordering;
use super::{current_task_and_cpu, validate_user_ptr};

/// Syscall: emit a debug value to the serial console.
///
/// Arguments: value (u64), tag (u64) — printed as "DBG[tag]: value"
pub fn sys_debug_log(value: u64, tag: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    log::info!("DBG[{:#x}]: {:#x}", tag, value);
    0
}

/// Syscall: read a key event (blocking).
///
/// Registers the current task as the keyboard waiter, sets it Sleeping,
/// enables interrupts, and halts. The keyboard ISR wakes it when a key arrives.
pub fn sys_read_key(key_event_out_ptr: u64, _: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    if !validate_user_ptr(key_event_out_ptr, core::mem::size_of::<kernel_api_types::KeyEvent>() as u64) {
        return 1;
    }
    let out = key_event_out_ptr as *mut kernel_api_types::KeyEvent;

    loop {
        if let Some(event) = crate::drivers::keyboard::try_read_key() {
            unsafe { core::ptr::write(out, event) };
            return 0;
        }

        // Set CpuContext.rax so the task sees a valid return value if woken early
        let ctx_ptr = get_local().current_context_ptr.load(Ordering::Relaxed);
        if !ctx_ptr.is_null() {
            unsafe { (*ctx_ptr).rax = 1; }
        }

        // Register waiter and sleep
        if let Some((task, cpu_id)) = current_task_and_cpu() {
            *crate::drivers::keyboard::KEYBOARD_WAITER.lock() = Some((task.clone(), cpu_id));
            task.state.store(TaskState::Sleeping, Ordering::Release);
        }

        x86_64::instructions::interrupts::enable();
        x86_64::instructions::hlt();
        x86_64::instructions::interrupts::disable();
    }
}

/// Syscall: try to read a mouse event (non-blocking).
///
/// Returns 0 and writes the event if one is available, or 1 if the buffer is empty.
pub fn sys_read_mouse(mouse_event_out_ptr: u64, _: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    if !validate_user_ptr(mouse_event_out_ptr, core::mem::size_of::<kernel_api_types::MouseEvent>() as u64) {
        return 1;
    }
    let out = mouse_event_out_ptr as *mut kernel_api_types::MouseEvent;

    match crate::drivers::mouse::try_read_mouse() {
        Some(event) => {
            unsafe { core::ptr::write(out, event) };
            0
        }
        None => 1,
    }
}

/// Syscall: load a Limine boot module by name.
///
/// Arguments: name_ptr, name_len, buf_ptr, buf_cap
///
/// Size query: if buf_ptr == 0 && buf_cap == 0, returns the module size (or 0 if not found).
/// Copy: copies module bytes to buf, returns bytes written (or 0 on failure).
pub fn sys_get_module(name_ptr: u64, name_len: u64, buf_ptr: u64, buf_cap: u64, _: u64, _: u64) -> u64 {
    if name_len == 0 || name_len > 256 {
        return 0;
    }
    if !validate_user_ptr(name_ptr, name_len) {
        return 0;
    }

    let name_bytes = unsafe { core::slice::from_raw_parts(name_ptr as *const u8, name_len as usize) };
    let name = match core::str::from_utf8(name_bytes) {
        Ok(s) => s,
        Err(_) => return 0,
    };

    // Build path by prepending "/" to name
    let mut path_buf = [0u8; 258];
    path_buf[0] = b'/';
    path_buf[1..1 + name.len()].copy_from_slice(name.as_bytes());
    let path = &path_buf[..1 + name.len()];

    let response = match MODULE_REQUEST.get_response() {
        Some(r) => r,
        None => return 0,
    };

    let module = match response.modules().iter().find(|m| m.path().to_bytes() == path) {
        Some(m) => m,
        None => return 0,
    };

    let module_size = module.size();

    // Size query mode
    if buf_ptr == 0 && buf_cap == 0 {
        return module_size;
    }

    if buf_cap < module_size {
        return 0;
    }
    if !validate_user_ptr(buf_ptr, buf_cap) {
        return 0;
    }

    unsafe {
        core::ptr::copy_nonoverlapping(
            module.addr() as *const u8,
            buf_ptr as *mut u8,
            module_size as usize,
        );
    }

    module_size
}
