use crate::memory::cpu_local_data::get_local;
use crate::task::task::TaskState;
use core::sync::atomic::Ordering;
use super::{current_task_and_cpu, validate_user_ptr};

/// Syscall: create a new IPC channel.
///
/// Arguments: send_ep_out_ptr, recv_ep_out_ptr, capacity
/// Writes the two endpoint IDs to the output pointers.
/// Returns: IPC status code.
pub fn sys_channel_create(send_ep_out_ptr: u64, recv_ep_out_ptr: u64, capacity: u64, _: u64, _: u64, _: u64) -> u64 {
    if !validate_user_ptr(send_ep_out_ptr, 8) || !validate_user_ptr(recv_ep_out_ptr, 8) {
        return kernel_api_types::IPC_ERR_INVALID_ARGS;
    }

    let cap = if capacity == 0 {
        crate::ipc::DEFAULT_CHANNEL_CAPACITY
    } else {
        (capacity as usize).clamp(1, crate::ipc::MAX_CHANNEL_CAPACITY)
    };

    let (send_id, recv_id) = crate::ipc::create_channel(cap);

    unsafe {
        core::ptr::write(send_ep_out_ptr as *mut u64, send_id);
        core::ptr::write(recv_ep_out_ptr as *mut u64, recv_id);
    }

    // Track endpoints for cleanup on exit
    {
        let cpu = get_local();
        let rq = cpu.run_queue.get().unwrap().lock();
        if let Some(t) = &rq.current_task {
            let mut inner = t.inner.lock();
            inner.owned_endpoints.push(send_id);
            inner.owned_endpoints.push(recv_id);
        }
    }

    kernel_api_types::IPC_OK
}

/// Syscall: send a message on a channel endpoint.
///
/// Blocks (via sleep+hlt) if the channel is full, woken by the receiver.
pub fn sys_channel_send(endpoint_id: u64, msg_ptr: u64, msg_len: u64, _: u64, _: u64, _: u64) -> u64 {
    if msg_len > crate::ipc::MAX_MESSAGE_SIZE as u64 {
        return kernel_api_types::IPC_ERR_MSG_TOO_LARGE;
    }
    if msg_len > 0 && !validate_user_ptr(msg_ptr, msg_len) {
        return kernel_api_types::IPC_ERR_INVALID_ARGS;
    }

    let data = if msg_len > 0 {
        unsafe { core::slice::from_raw_parts(msg_ptr as *const u8, msg_len as usize) }
    } else {
        &[]
    };

    // Get the channel Arc once so we can access its send_waiter on ChannelFull
    let channel_arc = {
        let registry = crate::ipc::ENDPOINT_REGISTRY.lock();
        match registry.get(&endpoint_id) {
            Some(ep) if ep.role == crate::ipc::EndpointRole::Send => ep.channel.clone(),
            Some(_) => return kernel_api_types::IPC_ERR_WRONG_DIRECTION,
            None => return kernel_api_types::IPC_ERR_INVALID_ENDPOINT,
        }
    };

    loop {
        match crate::ipc::try_send(endpoint_id, data) {
            Ok(()) => return kernel_api_types::IPC_OK,
            Err(crate::ipc::IpcError::ChannelFull) => {
                // Set fallback return value in CpuContext
                let ctx_ptr = get_local().current_context_ptr.load(Ordering::Relaxed);
                if !ctx_ptr.is_null() {
                    unsafe { (*ctx_ptr).rax = kernel_api_types::IPC_ERR_CHANNEL_FULL; }
                }
                // Register as send waiter and sleep
                if let Some((task, cpu_id)) = current_task_and_cpu() {
                    *channel_arc.send_waiter.lock() = Some((task.clone(), cpu_id));
                    task.state.store(TaskState::Sleeping, Ordering::Release);
                }
                x86_64::instructions::interrupts::enable();
                x86_64::instructions::hlt();
                x86_64::instructions::interrupts::disable();
            }
            Err(e) => return ipc_error_to_code(e),
        }
    }
}

/// Syscall: receive a message from a channel endpoint.
///
/// Blocks (via sleep+hlt) if channel is empty, woken by the sender.
pub fn sys_channel_recv(endpoint_id: u64, buf_ptr: u64, buf_cap: u64, bytes_read_out_ptr: u64, _: u64, _: u64) -> u64 {
    if !validate_user_ptr(buf_ptr, buf_cap) || !validate_user_ptr(bytes_read_out_ptr, 8) {
        return kernel_api_types::IPC_ERR_INVALID_ARGS;
    }

    // Get the channel Arc once so we can access its recv_waiter on WouldBlock
    let channel_arc = {
        let registry = crate::ipc::ENDPOINT_REGISTRY.lock();
        match registry.get(&endpoint_id) {
            Some(ep) if ep.role == crate::ipc::EndpointRole::Recv => ep.channel.clone(),
            Some(_) => return kernel_api_types::IPC_ERR_WRONG_DIRECTION,
            None => return kernel_api_types::IPC_ERR_INVALID_ENDPOINT,
        }
    };

    loop {
        match crate::ipc::try_recv(endpoint_id) {
            Ok(msg) => {
                let copy_len = msg.len().min(buf_cap as usize);
                unsafe {
                    core::ptr::copy_nonoverlapping(msg.as_ptr(), buf_ptr as *mut u8, copy_len);
                    core::ptr::write(bytes_read_out_ptr as *mut u64, copy_len as u64);
                }
                return kernel_api_types::IPC_OK;
            }
            Err(crate::ipc::IpcError::WouldBlock) => {
                // Set fallback return value in CpuContext (EINTR/EAGAIN semantics)
                let ctx_ptr = get_local().current_context_ptr.load(Ordering::Relaxed);
                if !ctx_ptr.is_null() {
                    unsafe { (*ctx_ptr).rax = kernel_api_types::IPC_ERR_CHANNEL_FULL; }
                }
                // Register as recv waiter and sleep
                if let Some((task, cpu_id)) = current_task_and_cpu() {
                    *channel_arc.recv_waiter.lock() = Some((task.clone(), cpu_id));
                    task.state.store(TaskState::Sleeping, Ordering::Release);
                }
                x86_64::instructions::interrupts::enable();
                x86_64::instructions::hlt();
                x86_64::instructions::interrupts::disable();
            }
            Err(e) => return ipc_error_to_code(e),
        }
    }
}

/// Syscall: close a channel endpoint.
///
/// Arguments: endpoint_id
/// Returns: IPC status code.
pub fn sys_channel_close(endpoint_id: u64, _: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    match crate::ipc::close_endpoint(endpoint_id) {
        Ok(()) => kernel_api_types::IPC_OK,
        Err(e) => ipc_error_to_code(e),
    }
}

fn ipc_error_to_code(e: crate::ipc::IpcError) -> u64 {
    match e {
        crate::ipc::IpcError::InvalidEndpoint => kernel_api_types::IPC_ERR_INVALID_ENDPOINT,
        crate::ipc::IpcError::WrongDirection  => kernel_api_types::IPC_ERR_WRONG_DIRECTION,
        crate::ipc::IpcError::PeerClosed      => kernel_api_types::IPC_ERR_PEER_CLOSED,
        crate::ipc::IpcError::ChannelFull     => kernel_api_types::IPC_ERR_CHANNEL_FULL,
        crate::ipc::IpcError::WouldBlock      => kernel_api_types::IPC_ERR_CHANNEL_FULL,
        crate::ipc::IpcError::MessageTooLarge => kernel_api_types::IPC_ERR_MSG_TOO_LARGE,
        crate::ipc::IpcError::InvalidArgs     => kernel_api_types::IPC_ERR_INVALID_ARGS,
    }
}
