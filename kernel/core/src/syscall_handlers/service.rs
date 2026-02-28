use kernel_api_types::{MAX_SERVICE_NAME_LEN, SVC_ERR_INVALID_ARGS, SVC_ERR_NOT_FOUND, SVC_OK};
use crate::ipc::{EndpointRole, ENDPOINT_REGISTRY};

pub fn sys_register_service(name_ptr: u64, name_len: u64, send_ep: u64, _: u64, _: u64, _: u64) -> u64 {
    if name_len == 0 || name_len > MAX_SERVICE_NAME_LEN as u64 {
        return SVC_ERR_INVALID_ARGS;
    }
    if !super::validate_user_ptr(name_ptr, name_len) {
        return SVC_ERR_INVALID_ARGS;
    }

    // Verify send_ep exists and is a Send endpoint
    {
        let registry = ENDPOINT_REGISTRY.lock();
        match registry.get(&send_ep) {
            Some(ep) if ep.role == EndpointRole::Send => {}
            _ => return SVC_ERR_INVALID_ARGS,
        }
    }

    let name_bytes = unsafe {
        core::slice::from_raw_parts(name_ptr as *const u8, name_len as usize)
    };

    let (task, task_id) = match super::current_task_and_cpu() {
        Some((t, cpu)) => {
            let id = t.id;
            (t, (id, cpu))
        }
        None => return SVC_ERR_INVALID_ARGS,
    };

    match crate::service_registry::register(name_bytes, send_ep, task_id.0) {
        Ok(()) => {
            // Record the registration in the task so sys_exit can clean it up
            let mut name_arr = [0u8; MAX_SERVICE_NAME_LEN];
            let copy_len = name_bytes.len().min(MAX_SERVICE_NAME_LEN);
            name_arr[..copy_len].copy_from_slice(&name_bytes[..copy_len]);
            task.inner.lock().registered_services.push(name_arr);
            SVC_OK
        }
        Err(code) => code,
    }
}

pub fn sys_lookup_service(name_ptr: u64, name_len: u64, ep_out_ptr: u64, _: u64, _: u64, _: u64) -> u64 {
    if name_len == 0 || name_len > MAX_SERVICE_NAME_LEN as u64 {
        return SVC_ERR_INVALID_ARGS;
    }
    if !super::validate_user_ptr(name_ptr, name_len) {
        return SVC_ERR_INVALID_ARGS;
    }
    if !super::validate_user_ptr(ep_out_ptr, 8) {
        return SVC_ERR_INVALID_ARGS;
    }

    let name_bytes = unsafe {
        core::slice::from_raw_parts(name_ptr as *const u8, name_len as usize)
    };

    match crate::service_registry::lookup(name_bytes) {
        Some(id) => {
            unsafe { core::ptr::write(ep_out_ptr as *mut u64, id) };
            SVC_OK
        }
        None => SVC_ERR_NOT_FOUND,
    }
}
