use crate::memory::cpu_local_data::get_local;
use crate::task::global_scheduler::TASK_TABLE;
use crate::task::task::{TaskId, TaskKind, TaskState};
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
/// Arguments: elf_ptr, elf_len, child_arg
/// Returns: task ID on success, 0 on failure.
pub fn sys_spawn(elf_ptr: u64, elf_len: u64, child_arg: u64, _: u64, _: u64, _: u64) -> u64 {
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

    match crate::user_task_from_elf::create_user_task_from_elf_bytes(elf_bytes, child_arg) {
        Ok(task) => {
            let id = task.id.to_u64();
            crate::task::global_scheduler::spawn_task(task);
            id
        }
        Err(_) => 0,
    }
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
