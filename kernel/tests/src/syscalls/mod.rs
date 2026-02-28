/// Syscall API tests
///
/// Two categories:
/// 1. Argument-validation tests — no user task needed; verify error codes for
///    bad inputs (null pointers, invalid endpoint IDs, oversized messages, …).
/// 2. Full-flow tests — use `with_user_context` which installs a user task as
///    the CPU's current task and switches to its page table, mirroring the
///    environment syscall handlers normally execute in.
use crate::TestResult;
use alloc::{format, sync::Arc};
use kernel::ipc;
use kernel::memory::cpu_local_data::get_local;
use kernel::user_task_from_elf::create_user_task_from_elf_bytes;
use kernel_api_types::{
    graphics::GraphicsResult, IPC_ERR_INVALID_ARGS, IPC_ERR_INVALID_ENDPOINT,
    IPC_ERR_MSG_TOO_LARGE, IPC_ERR_WRONG_DIRECTION, IPC_OK, MMAP_WRITE,
};
use x86_64::instructions::interrupts;
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::{PhysFrame, Size4KiB};
use x86_64::PhysAddr;

// ─── helpers ────────────────────────────────────────────────────────────────

fn get_init_task_elf() -> &'static [u8] {
    use core::ptr::{slice_from_raw_parts_mut, NonNull};
    let module = kernel::limine_requests::MODULE_REQUEST
        .get_response()
        .unwrap()
        .modules()
        .iter()
        .find(|m| m.path() == kernel::limine_requests::INIT_TASK_PATH)
        .expect("init_task module not found");
    let ptr =
        NonNull::new(slice_from_raw_parts_mut(module.addr(), module.size() as usize)).unwrap();
    unsafe { ptr.as_ref() }
}

/// Run `f` in the syscall environment:
/// - interrupts disabled (SFMask clears IF on SYSCALL)
/// - a `TaskKind::User` task is installed as `current_task`
/// - CR3 = user page table, so user virtual addresses are accessible
///
/// The kernel higher-half is shared with the user page table, so kernel code,
/// GS.Base, and the stack remain accessible throughout the switch.
fn with_user_context(f: impl FnOnce() -> TestResult) -> TestResult {
    let task = match create_user_task_from_elf_bytes(get_init_task_elf(), 0) {
        Ok(t) => Arc::new(t),
        Err(e) => {
            return TestResult::Failed(format!("failed to create user task: {:?}", e));
        }
    };

    interrupts::without_interrupts(|| {
        {
            let cpu = get_local();
            let mut rq = cpu.run_queue.get().unwrap().lock();
            rq.current_task = Some(task.clone());
        }

        let (kernel_frame, cr3_flags) = Cr3::read();
        let user_frame =
            PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(task.cr3));
        unsafe { Cr3::write(user_frame, cr3_flags) };

        let result = f();

        unsafe { Cr3::write(kernel_frame, cr3_flags) };

        {
            let cpu = get_local();
            let mut rq = cpu.run_queue.get().unwrap().lock();
            rq.current_task = None;
        }

        result
    })
}

// ─── 1. Argument-validation tests (no user context needed) ──────────────────

/// sys_debug_log always succeeds regardless of arguments.
pub fn test_sys_debug_log_always_ok() -> TestResult {
    let ret = kernel::syscall_handlers::sys_debug_log(0xDEAD_BEEF, 0xCAFE_BABE, 0, 0, 0, 0);
    if ret != 0 {
        return TestResult::Failed(format!("sys_debug_log returned {ret:#x}, expected 0"));
    }
    TestResult::Ok
}

/// Null send_ep_out pointer → IPC_ERR_INVALID_ARGS.
pub fn test_sys_channel_create_null_send_ptr() -> TestResult {
    let mut dummy: u64 = 0;
    let ret = kernel::syscall_handlers::sys_channel_create(
        0,
        &mut dummy as *mut u64 as u64,
        8,
        0,
        0,
        0,
    );
    if ret != IPC_ERR_INVALID_ARGS {
        return TestResult::Failed(format!(
            "expected IPC_ERR_INVALID_ARGS ({IPC_ERR_INVALID_ARGS:#x}), got {ret:#x}"
        ));
    }
    TestResult::Ok
}

/// Null recv_ep_out pointer → IPC_ERR_INVALID_ARGS.
pub fn test_sys_channel_create_null_recv_ptr() -> TestResult {
    let mut dummy: u64 = 0;
    let ret = kernel::syscall_handlers::sys_channel_create(
        &mut dummy as *mut u64 as u64,
        0,
        8,
        0,
        0,
        0,
    );
    if ret != IPC_ERR_INVALID_ARGS {
        return TestResult::Failed(format!(
            "expected IPC_ERR_INVALID_ARGS ({IPC_ERR_INVALID_ARGS:#x}), got {ret:#x}"
        ));
    }
    TestResult::Ok
}

/// Send to a non-existent endpoint (msg_len=0 skips the buf-pointer check)
/// → IPC_ERR_INVALID_ENDPOINT.
pub fn test_sys_channel_send_invalid_endpoint() -> TestResult {
    let ret = kernel::syscall_handlers::sys_channel_send(99999, 0, 0, 0, 0, 0);
    if ret != IPC_ERR_INVALID_ENDPOINT {
        return TestResult::Failed(format!(
            "expected IPC_ERR_INVALID_ENDPOINT ({IPC_ERR_INVALID_ENDPOINT:#x}), got {ret:#x}"
        ));
    }
    TestResult::Ok
}

/// Message larger than MAX_MESSAGE_SIZE → IPC_ERR_MSG_TOO_LARGE (checked before
/// any pointer validation, so no user context is needed).
pub fn test_sys_channel_send_too_large() -> TestResult {
    let ret = kernel::syscall_handlers::sys_channel_send(
        1,
        0x1000,
        (ipc::MAX_MESSAGE_SIZE + 1) as u64,
        0,
        0,
        0,
    );
    if ret != IPC_ERR_MSG_TOO_LARGE {
        return TestResult::Failed(format!(
            "expected IPC_ERR_MSG_TOO_LARGE ({IPC_ERR_MSG_TOO_LARGE:#x}), got {ret:#x}"
        ));
    }
    TestResult::Ok
}

/// Null buf_ptr → IPC_ERR_INVALID_ARGS on channel_recv.
pub fn test_sys_channel_recv_null_ptr() -> TestResult {
    let ret = kernel::syscall_handlers::sys_channel_recv(1, 0, 0, 0, 0, 0);
    if ret != IPC_ERR_INVALID_ARGS {
        return TestResult::Failed(format!(
            "expected IPC_ERR_INVALID_ARGS ({IPC_ERR_INVALID_ARGS:#x}), got {ret:#x}"
        ));
    }
    TestResult::Ok
}

/// Closing a non-existent endpoint must not return IPC_OK.
pub fn test_sys_channel_close_invalid_endpoint() -> TestResult {
    let ret = kernel::syscall_handlers::sys_channel_close(99999, 0, 0, 0, 0, 0);
    if ret == IPC_OK {
        return TestResult::Failed("closing a non-existent endpoint returned IPC_OK".into());
    }
    TestResult::Ok
}

/// sys_mmap(0, …) returns 0 — zero-size allocation is rejected before any
/// task-context check.
pub fn test_sys_mmap_zero_size() -> TestResult {
    let ret = kernel::syscall_handlers::sys_mmap(0, MMAP_WRITE, 0, 0, 0, 0);
    if ret != 0 {
        return TestResult::Failed(format!("sys_mmap(0) returned {ret:#x}, expected 0"));
    }
    TestResult::Ok
}

/// sys_munmap with an unaligned address returns an error before any task-context check.
pub fn test_sys_munmap_unaligned() -> TestResult {
    let ret = kernel::syscall_handlers::sys_munmap(1, 4096, 0, 0, 0, 0);
    if ret == 0 {
        return TestResult::Failed(
            "sys_munmap with unaligned addr returned 0 (success)".into(),
        );
    }
    TestResult::Ok
}

/// Sending on a recv-endpoint returns IPC_ERR_WRONG_DIRECTION.
/// Receiving on a send-endpoint is rejected (either WRONG_DIRECTION or INVALID_ARGS
/// from the null-ptr check — either way, not IPC_OK).
pub fn test_sys_channel_wrong_direction() -> TestResult {
    let (send_id, recv_id) = ipc::create_channel(4);

    // msg_len=0 skips buf-ptr validation, so the direction check is reached
    let send_on_recv = kernel::syscall_handlers::sys_channel_send(recv_id, 0, 0, 0, 0, 0);
    // null buf_ptr triggers INVALID_ARGS before the direction check on recv
    let recv_on_send = kernel::syscall_handlers::sys_channel_recv(send_id, 0, 0, 0, 0, 0);

    let _ = ipc::close_endpoint(send_id);
    let _ = ipc::close_endpoint(recv_id);

    if send_on_recv != IPC_ERR_WRONG_DIRECTION {
        return TestResult::Failed(format!(
            "send on recv-endpoint: expected IPC_ERR_WRONG_DIRECTION ({IPC_ERR_WRONG_DIRECTION:#x}), got {send_on_recv:#x}"
        ));
    }
    if recv_on_send == IPC_OK {
        return TestResult::Failed("recv on send-endpoint returned IPC_OK".into());
    }
    TestResult::Ok
}

// ─── 2. Full-flow tests (user task context + user CR3) ──────────────────────

/// sys_channel_create writes two non-zero, distinct endpoint IDs into user memory
/// and returns IPC_OK.
pub fn test_sys_channel_create_returns_endpoints() -> TestResult {
    with_user_context(|| {
        // 16 bytes: two u64 output slots
        let ep_buf = kernel::syscall_handlers::sys_mmap(16, MMAP_WRITE, 0, 0, 0, 0);
        if ep_buf == 0 {
            return TestResult::Failed("sys_mmap for ep_buf failed".into());
        }

        let ret =
            kernel::syscall_handlers::sys_channel_create(ep_buf, ep_buf + 8, 8, 0, 0, 0);
        let send_id = unsafe { core::ptr::read(ep_buf as *const u64) };
        let recv_id = unsafe { core::ptr::read((ep_buf + 8) as *const u64) };

        let _ = ipc::close_endpoint(send_id);
        let _ = ipc::close_endpoint(recv_id);

        if ret != IPC_OK {
            return TestResult::Failed(format!("sys_channel_create returned {ret:#x}"));
        }
        if send_id == 0 || recv_id == 0 {
            return TestResult::Failed(format!(
                "endpoint IDs must be non-zero: send={send_id}, recv={recv_id}"
            ));
        }
        if send_id == recv_id {
            return TestResult::Failed(format!("send_id == recv_id == {send_id}"));
        }
        TestResult::Ok
    })
}

/// sys_mmap returns a non-zero, page-aligned address within the user range.
pub fn test_sys_mmap_returns_valid_addr() -> TestResult {
    with_user_context(|| {
        let addr = kernel::syscall_handlers::sys_mmap(4096, MMAP_WRITE, 0, 0, 0, 0);
        if addr == 0 {
            return TestResult::Failed("sys_mmap returned 0".into());
        }
        if addr % 4096 != 0 {
            return TestResult::Failed(format!(
                "sys_mmap returned unaligned addr {addr:#x}"
            ));
        }
        if addr < kernel::consts::USER_MIN || addr > kernel::consts::USER_MAX {
            return TestResult::Failed(format!(
                "sys_mmap addr {addr:#x} is outside user range [{:#x}, {:#x}]",
                kernel::consts::USER_MIN,
                kernel::consts::USER_MAX,
            ));
        }
        TestResult::Ok
    })
}

/// sys_mmap + write + read back + sys_munmap.
pub fn test_sys_mmap_write_and_read() -> TestResult {
    with_user_context(|| {
        let addr = kernel::syscall_handlers::sys_mmap(4096, MMAP_WRITE, 0, 0, 0, 0);
        if addr == 0 {
            return TestResult::Failed("sys_mmap returned 0".into());
        }

        // User CR3 is active, so the user virtual address is directly accessible.
        const PATTERN: u64 = 0xDEAD_BEEF_CAFE_BABE;
        unsafe { core::ptr::write(addr as *mut u64, PATTERN) };
        let readback = unsafe { core::ptr::read(addr as *const u64) };

        let munmap_ret = kernel::syscall_handlers::sys_munmap(addr, 4096, 0, 0, 0, 0);

        if readback != PATTERN {
            return TestResult::Failed(format!(
                "write/read mismatch: wrote {PATTERN:#x}, read back {readback:#x}"
            ));
        }
        if munmap_ret != 0 {
            return TestResult::Failed(format!("sys_munmap returned {munmap_ret:#x}"));
        }
        TestResult::Ok
    })
}

/// sys_channel_send (empty message) + sys_channel_recv roundtrip via the
/// syscall layer.  An empty send bypasses the message-buffer pointer check
/// while still exercising the IPC path end-to-end.
pub fn test_sys_channel_send_recv_roundtrip() -> TestResult {
    with_user_context(|| {
        // Allocate user memory for recv buffer and bytes_read output
        let recv_buf = kernel::syscall_handlers::sys_mmap(64, MMAP_WRITE, 0, 0, 0, 0);
        if recv_buf == 0 {
            return TestResult::Failed("sys_mmap for recv_buf failed".into());
        }
        let bytes_out = kernel::syscall_handlers::sys_mmap(8, MMAP_WRITE, 0, 0, 0, 0);
        if bytes_out == 0 {
            return TestResult::Failed("sys_mmap for bytes_out failed".into());
        }

        // Create the channel via the internal API (avoids needing user ptrs for IDs)
        let (send_id, recv_id) = ipc::create_channel(4);

        // Send an empty message (msg_len=0 skips the message-buffer pointer check)
        let send_ret =
            kernel::syscall_handlers::sys_channel_send(send_id, 0, 0, 0, 0, 0);
        if send_ret != IPC_OK {
            let _ = ipc::close_endpoint(send_id);
            let _ = ipc::close_endpoint(recv_id);
            return TestResult::Failed(format!("sys_channel_send returned {send_ret:#x}"));
        }

        // Receive — the message is already in the queue so this returns immediately
        let recv_ret = kernel::syscall_handlers::sys_channel_recv(
            recv_id, recv_buf, 64, bytes_out, 0, 0,
        );
        let bytes_read = unsafe { core::ptr::read(bytes_out as *const u64) };

        let _ = ipc::close_endpoint(send_id);
        let _ = ipc::close_endpoint(recv_id);

        if recv_ret != IPC_OK {
            return TestResult::Failed(format!("sys_channel_recv returned {recv_ret:#x}"));
        }
        if bytes_read != 0 {
            return TestResult::Failed(format!(
                "expected 0 bytes for empty message, got {bytes_read}"
            ));
        }
        TestResult::Ok
    })
}

/// sys_get_display_info writes valid (non-zero) display dimensions into a user buffer.
pub fn test_sys_get_display_info_success() -> TestResult {
    with_user_context(|| {
        let info_size =
            core::mem::size_of::<kernel_api_types::graphics::DisplayInfo>() as u64;
        let info_buf = kernel::syscall_handlers::sys_mmap(info_size, MMAP_WRITE, 0, 0, 0, 0);
        if info_buf == 0 {
            return TestResult::Failed("sys_mmap for DisplayInfo buffer failed".into());
        }

        let ret =
            kernel::syscall_handlers::sys_get_display_info(info_buf, 0, 0, 0, 0, 0);

        if ret != GraphicsResult::Ok as u64 {
            return TestResult::Failed(format!("sys_get_display_info returned {ret:#x}"));
        }

        let info = unsafe {
            core::ptr::read(
                info_buf as *const kernel_api_types::graphics::DisplayInfo,
            )
        };
        if info.width == 0 || info.height == 0 {
            return TestResult::Failed(format!(
                "DisplayInfo has zero dimensions: {}×{}", info.width, info.height
            ));
        }
        TestResult::Ok
    })
}
