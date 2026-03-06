use crate::memory::cpu_local_data::get_local;
use crate::task::global_scheduler::TASK_TABLE;
use crate::task::task::{Task, TaskId, TaskKind, TaskState};
use core::sync::atomic::Ordering;
use super::{current_task_and_cpu, wake_task};

/// Syscall: exit the current task.
///
/// Closes owned IPC endpoints, stores the exit code, wakes any waitpid waiter
/// (or removes from TASK_TABLE immediately if detached), then halts.
pub fn sys_exit(exit_code: u64) -> ! {
    let cpu = get_local();

    // 1. Collect task Arc and owned endpoints (interrupts still disabled by SFMask)
    let (task_arc, endpoints) = {
        let rq = cpu.run_queue.get().unwrap().lock();
        let t = rq.current_task.clone();
        let eps = t.as_ref()
            .map(|t| t.inner.lock().owned_endpoints.clone())
            .unwrap_or_default();
        (t, eps)
    };

    // 2. Close owned IPC endpoints
    for ep in endpoints {
        let _ = crate::ipc::close_endpoint(ep);
    }

    // 2b. Unregister any services this task registered
    if let Some(task) = &task_arc {
        crate::service_registry::unregister_all_for_task(task.id);
    }

    // 3. Set exit code + Zombie, wake waiter or detach
    if let Some(task) = task_arc {
        task.exit_code.store(exit_code, Ordering::Release);
        task.state.store(TaskState::Zombie, Ordering::Relaxed);

        // Send exit notification if a channel endpoint was registered
        let notif_ep = task.exit_notification_ep.load(Ordering::Relaxed);
        if notif_ep != 0 {
            let mut msg = [0u8; 16];
            msg[0..8].copy_from_slice(&task.id.to_u64().to_le_bytes());
            msg[8..16].copy_from_slice(&exit_code.to_le_bytes());
            let _ = crate::ipc::try_send(notif_ep, &msg);
        }

        if let Some((waiter, w_cpu)) = task.exit_waiter.lock().take() {
            wake_task(waiter, w_cpu);
        } else {
            // No waiter: detached task — remove immediately
            TASK_TABLE.lock().remove(&task.id);
        }
    }

    x86_64::instructions::interrupts::enable();
    loop {
        x86_64::instructions::hlt();
    }
}

/// Syscall: yield the current timeslice.
///
/// Enables interrupts and halts — the timer interrupt will immediately reschedule.
/// When the timer preempts us here, it sees in_syscall=1 and uses the CpuContext
/// that was saved at syscall entry. We set rax in CpuContext to the return value
/// so the user task sees the correct result when it resumes via iretq.
pub fn sys_yield(_: u64, _: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    let ctx_ptr = get_local().current_context_ptr.load(Ordering::Relaxed);
    if !ctx_ptr.is_null() {
        unsafe { (*ctx_ptr).rax = 0; }
    }
    x86_64::instructions::interrupts::enable();
    x86_64::instructions::hlt();
    x86_64::instructions::interrupts::disable();
    0
}

/// Syscall: spawn a new user task from ELF bytes in the caller's memory.
///
/// Arguments: elf_ptr, elf_len, child_arg, name_ptr, name_len
/// Returns: task ID on success, 0 on failure.
pub fn sys_spawn(elf_ptr: u64, elf_len: u64, child_arg: u64, name_ptr: u64, name_len: u64, _: u64) -> u64 {
    if elf_len == 0 || elf_len > 64 * 1024 * 1024 {
        return 0;
    }
    if !super::validate_user_ptr(elf_ptr, elf_len) {
        return 0;
    }

    let cpu = get_local();
    {
        let rq = cpu.run_queue.get().unwrap().lock();
        match &rq.current_task {
            Some(t) if t.kind == TaskKind::User => {}
            _ => return 0,
        }
    }

    let elf_bytes = unsafe {
        core::slice::from_raw_parts(elf_ptr as *const u8, elf_len as usize)
    };

    let mut name_buf = [0u8; 32];
    let name: &[u8] = if name_ptr != 0 && name_len != 0 {
        let capped = name_len.min(32) as usize;
        if super::validate_user_ptr(name_ptr, capped as u64) {
            unsafe { core::ptr::copy_nonoverlapping(name_ptr as *const u8, name_buf.as_mut_ptr(), capped); }
            &name_buf[..capped]
        } else {
            b""
        }
    } else {
        b""
    };

    match crate::user_task_from_elf::create_user_task_from_elf_bytes(elf_bytes, child_arg, name) {
        Ok(task) => {
            let id = task.id.to_u64();
            let name_str = core::str::from_utf8(name).unwrap_or("?");
            log::info!("sys_spawn: ok task={} name={:?} entry={:#x}", id, name_str, elf_bytes.as_ptr() as u64);
            crate::task::global_scheduler::spawn_task(task);
            id
        }
        Err(e) => {
            let name_str = core::str::from_utf8(name).unwrap_or("?");
            log::warn!("sys_spawn: FAILED name={:?} err={:?} elf_len={}", name_str, e, elf_len);
            0
        }
    }
}

/// Syscall: sleep the current task for at least `ms` milliseconds.
pub fn sys_sleep_ms(ms: u64, _: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    if ms == 0 { return 0; }
    super::event::sys_wait_for_event(0, 0, 0, ms, 0, 0);
    0
}

/// Syscall: create a new thread sharing the caller's address space.
///
/// Arguments: entry_rip, stack_top, arg, name_ptr, name_len
/// Returns: task ID on success, 0 on failure.
pub fn sys_thread_create(entry: u64, stack_top: u64, arg: u64, name_ptr: u64, name_len: u64, _: u64) -> u64 {
    let cpu = get_local();

    let (parent_cr3, vaddr_set) = {
        let rq = cpu.run_queue.get().unwrap().lock();
        let task = match &rq.current_task {
            Some(t) if t.kind == TaskKind::User => t.clone(),
            _ => return 0,
        };
        let inner = task.inner.lock();
        (task.cr3, inner.user_vaddr_set.clone())
    };

    let mut name_buf = [0u8; 32];
    let name: &[u8] = if name_ptr != 0 && name_len != 0 {
        let capped = name_len.min(32) as usize;
        if super::validate_user_ptr(name_ptr, capped as u64) {
            unsafe { core::ptr::copy_nonoverlapping(name_ptr as *const u8, name_buf.as_mut_ptr(), capped); }
            &name_buf[..capped]
        } else {
            b""
        }
    } else {
        b""
    };

    let gdt = cpu.gdt.get().unwrap();
    let user_cs = gdt.user_code_selector().0;
    let user_ss = gdt.user_data_selector().0;

    let task = Task::new_thread(entry, stack_top, parent_cr3, user_cs, user_ss, vaddr_set, arg, name);
    let id = task.id.to_u64();
    crate::task::global_scheduler::spawn_task(task);
    id
}

/// Syscall: register a send endpoint to receive an exit notification when `task_id` exits.
///
/// Arguments: task_id, send_ep_id
/// Returns: 0 on success, 1 on error.
pub fn sys_set_exit_channel(task_id: u64, send_ep_id: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    use crate::ipc::{ENDPOINT_REGISTRY, EndpointRole};

    {
        let registry = ENDPOINT_REGISTRY.lock();
        match registry.get(&send_ep_id) {
            Some(ep) if ep.role == EndpointRole::Send => {}
            _ => return 1,
        }
    }

    let target = match TASK_TABLE.lock().get(&TaskId::from_u64(task_id)).cloned() {
        Some(t) => t,
        None => return 1,
    };
    target.exit_notification_ep.store(send_ep_id, Ordering::Relaxed);
    0
}

/// Syscall: wait for a task to exit and collect its exit code.
///
/// Arguments: target_task_id, exit_code_out_ptr
/// Returns: 0 on success, 1 on error (task not found or invalid pointer).
pub fn sys_waitpid(target_id: u64, exit_code_out_ptr: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    if !super::validate_user_ptr(exit_code_out_ptr, 8) {
        return 1;
    }

    loop {
        let target = TASK_TABLE.lock().get(&TaskId::from_u64(target_id)).cloned();
        let target = match target {
            Some(t) => t,
            None => return 1,
        };

        if target.state.load(Ordering::Acquire) == TaskState::Zombie {
            let code = target.exit_code.load(Ordering::Acquire);
            TASK_TABLE.lock().remove(&TaskId::from_u64(target_id));
            unsafe { core::ptr::write(exit_code_out_ptr as *mut u64, code) };
            return 0;
        }

        // Target still alive: register as exit waiter and sleep
        if let Some((self_task, cpu_id)) = current_task_and_cpu() {
            *target.exit_waiter.lock() = Some((self_task.clone(), cpu_id));
            self_task.state.store(TaskState::Sleeping, Ordering::Release);
        }

        x86_64::instructions::interrupts::enable();
        x86_64::instructions::hlt();
        x86_64::instructions::interrupts::disable();
    }
}
