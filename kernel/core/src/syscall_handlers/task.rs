use crate::memory::cpu_local_data::get_local;
use crate::memory::MEMORY;
use crate::task::global_scheduler::{TASK_TABLE, preregister_task, spawn_task, spawn_task_activate};
use crate::task::task::{Task, TaskId, TaskKind, TaskState};
use crate::user_task_from_elf::{ElfLoaderArgs, fill_loading_task};
use alloc::boxed::Box;
use core::sync::atomic::Ordering;
use kernel_api_types::Priority;
use x86_64::registers::control::Cr3;
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

        // Free user page tables eagerly while in syscall context (interrupts disabled,
        // no spinlocks held). Idempotent: TaskInner::drop skips the free if take() → None.
        // This avoids relying on the scheduler's Arc drop, which only runs when another
        // task is ready — if no task is ready the zombie could hold frames indefinitely.
        task.free_address_space_now();

        if let Some((waiter, w_cpu)) = task.exit_waiter.lock().take() {
            wake_task(waiter, w_cpu);
        } else if task.kind == TaskKind::User {
            // User task, detached: remove from TASK_TABLE immediately.
            // Safe: user tasks run in user mode when preempted, so the kernel
            // allocator isn't held — GuardedStack::drop in the ISR is fine.
            TASK_TABLE.lock().remove(&task.id);
        }
        // Kernel task, detached: leave in TASK_TABLE as Zombie.
        // The idle task's reap_zombie_kernel_tasks() will remove it in task
        // context, where GuardedStack::drop can safely call the allocator.

        // Switch to kernel CR3 so we do not leave the CPU pointing at the freed L4
        // frame. The scheduler will set it correctly when it picks the next task.
        if task.kind == TaskKind::User {
            let mem = MEMORY.get().unwrap();
            unsafe { Cr3::write(mem.new_kernel_cr3, mem.new_kernel_cr3_flags); }
        }
    }

    x86_64::instructions::interrupts::enable();
    loop {
        x86_64::instructions::hlt();
    }
}

/// Kill the current user task from a hardware exception handler (page fault, GPF, #DE).
///
/// Called after the exception handler has already done `swapgs` so `get_local()`
/// is safe. Performs the same resource cleanup as `sys_exit`, sends a `FaultEvent`
/// to the task's `fault_ep` (if registered), then halts; the timer will reschedule
/// to the next ready task.
///
/// `fault_type` — one of the `FAULT_*` constants from `kernel_api_types`.
/// `faulting_addr` — the faulting virtual address (CR2 for page faults; 0 otherwise).
/// `ip` — instruction pointer at the time of the fault.
pub(crate) fn kill_from_exception(fault_type: u64, faulting_addr: u64, ip: u64) -> ! {
    let cpu = get_local();

    let (task_arc, endpoints) = {
        let rq = cpu.run_queue.get().unwrap().lock();
        let t = rq.current_task.clone();
        if let Some(task) = &t {
            let name = core::str::from_utf8(&task.name[..task.name_len as usize]).unwrap_or("?");
            log::warn!("kill_from_exception: task {} ({}) fault_type={} addr={:#x} ip={:#x}",
                task.id.to_u64(), name, fault_type, faulting_addr, ip);
        } else {
            log::warn!("kill_from_exception: no current task fault_type={} addr={:#x} ip={:#x}",
                fault_type, faulting_addr, ip);
        }
        let eps = t.as_ref()
            .map(|t| t.inner.lock().owned_endpoints.clone())
            .unwrap_or_default();
        (t, eps)
    };

    for ep in endpoints {
        let _ = crate::ipc::close_endpoint(ep);
    }

    if let Some(task) = &task_arc {
        crate::service_registry::unregister_all_for_task(task.id);
    }

    if let Some(task) = task_arc {
        task.exit_code.store(fault_type, Ordering::Release);

        // Notify fault_ep (if set) before marking Zombie so the receiver can
        // still inspect the task if needed.
        let fault_ep = task.fault_ep.load(Ordering::Relaxed);
        if fault_ep != 0 {
            let event = kernel_api_types::FaultEvent {
                task_id: task.id.to_u64(),
                fault_type,
                faulting_addr,
                instruction_pointer: ip,
            };
            let bytes: &[u8] = unsafe {
                core::slice::from_raw_parts(
                    &event as *const kernel_api_types::FaultEvent as *const u8,
                    core::mem::size_of::<kernel_api_types::FaultEvent>(),
                )
            };
            let _ = crate::ipc::try_send(fault_ep, bytes);
        }

        task.state.store(TaskState::Zombie, Ordering::Relaxed);

        let notif_ep = task.exit_notification_ep.load(Ordering::Relaxed);
        if notif_ep != 0 {
            let mut msg = [0u8; 16];
            msg[0..8].copy_from_slice(&task.id.to_u64().to_le_bytes());
            msg[8..16].copy_from_slice(&fault_type.to_le_bytes());
            let _ = crate::ipc::try_send(notif_ep, &msg);
        }

        task.free_address_space_now();

        if let Some((waiter, w_cpu)) = task.exit_waiter.lock().take() {
            wake_task(waiter, w_cpu);
        } else {
            TASK_TABLE.lock().remove(&task.id);
        }

        // Switch to kernel CR3 before we return to hlt — the user L4 frame was freed.
        let mem = MEMORY.get().unwrap();
        unsafe { Cr3::write(mem.new_kernel_cr3, mem.new_kernel_cr3_flags); }
    }

    x86_64::instructions::interrupts::enable();
    loop {
        x86_64::instructions::hlt();
    }
}

/// Syscall: register a send endpoint to receive a `FaultEvent` when `task_id` faults.
///
/// Arguments: task_id, send_ep_id
/// Returns: 0 on success, 1 on error (task not found or invalid endpoint).
pub fn sys_set_fault_ep(task_id: u64, send_ep_id: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
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
    target.fault_ep.store(send_ep_id, Ordering::Relaxed);
    0
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
/// Returns a valid child `TaskId` immediately. The child is registered in
/// `TASK_TABLE` as a `Loading` stub while a dedicated kernel loader task
/// performs the ELF parse and address-space setup asynchronously.
///
/// Arguments: elf_ptr, elf_len, child_arg, name_ptr, name_len, requested_priority
/// Returns: task ID on success, 0 on failure.
pub fn sys_spawn(elf_ptr: u64, elf_len: u64, child_arg: u64, name_ptr: u64, name_len: u64, requested_priority: u64) -> u64 {
    if elf_len == 0 || elf_len > 64 * 1024 * 1024 {
        return 0;
    }
    if !super::validate_user_ptr(elf_ptr, elf_len) {
        return 0;
    }

    let cpu = get_local();
    let (parent_task, parent_id) = {
        let rq = cpu.run_queue.get().unwrap().lock();
        match rq.current_task.clone() {
            Some(t) if t.kind == TaskKind::User => {
                let id = t.id;
                (t, id)
            }
            _ => return 0,
        }
    };

    // Clamp requested priority to at most parent's priority
    let parent_prio = Priority::from_u8(parent_task.priority.load(Ordering::Relaxed));
    let prio = Priority::from_u8(requested_priority as u8).min(parent_prio);

    // Capture the parent's CR3 so the loader can read ELF pages directly
    let parent_cr3 = parent_task.cr3.load(Ordering::Relaxed);

    // Extract name
    let mut name_arr = [0u8; 32];
    let name_len_capped: u8 = if name_ptr != 0 && name_len != 0 {
        let capped = name_len.min(32) as usize;
        if super::validate_user_ptr(name_ptr, capped as u64) {
            unsafe { core::ptr::copy_nonoverlapping(name_ptr as *const u8, name_arr.as_mut_ptr(), capped); }
            capped as u8
        } else {
            0
        }
    } else {
        0
    };

    // Create Loading stub and register in TASK_TABLE immediately
    let child_id = TaskId::alloc();
    let stub = Task::new_loading(child_id, name_arr, name_len_capped, prio, Some(parent_id));
    preregister_task(stub.clone());

    // Spawn a kernel loader task that fills the stub asynchronously
    let args = Box::new(ElfLoaderArgs {
        stub_task: stub,
        parent_cr3,
        elf_user_ptr: elf_ptr,
        elf_len,
        child_arg,
        name: name_arr,
        name_len: name_len_capped,
        priority: prio as u8,
        parent_id,
    });
    let args_ptr = Box::into_raw(args) as u64;
    let loader = Task::new(elf_loader_entry, args_ptr, Priority::Normal, None);
    spawn_task(loader);

    child_id.to_u64()
}

/// Naked trampoline for the kernel ELF loader task.
///
/// `Task::new` stores the args pointer in `CpuContext.rdi` and the entry address
/// in `r15`. The task trampoline does `call r15`, preserving `rdi`. This naked
/// function has no compiler-generated prologue, so `rdi` is guaranteed untouched
/// when we jump to the inner function which receives it as its first SysV argument.
#[unsafe(naked)]
fn elf_loader_entry() -> ! {
    core::arch::naked_asm!(
        "jmp {inner}",
        inner = sym elf_loader_entry_inner,
    )
}

/// Inner function: parses an ELF and fills a Loading stub asynchronously.
///
/// Receives `args_ptr` (a `Box<ElfLoaderArgs>` as u64) via the first SysV
/// argument register (`rdi`). On success, transitions the stub to Ready.
/// On failure, marks it Zombie and wakes any waitpid waiter.
extern "sysv64" fn elf_loader_entry_inner(args_ptr: u64) -> ! {
    let args = unsafe { *Box::from_raw(args_ptr as *mut ElfLoaderArgs) };
    let stub = args.stub_task.clone();

    let result = fill_loading_task(
        &stub,
        args.parent_cr3,
        args.elf_user_ptr,
        args.elf_len,
        args.child_arg,
        args.name,
        args.name_len,
        args.priority,
        args.parent_id,
    );

    // Free args heap allocations NOW, before sys_exit.
    // sys_exit() is `-> !` and never returns, so Rust drop glue does not run for
    // any local variable that is still live at the call site.
    drop(args);

    match result {
        Ok(()) => {
            log::info!("async ELF load complete for task {}", stub.id.to_u64());
            spawn_task_activate(stub); // consumes the Arc
        }
        Err(e) => {
            log::warn!("async ELF load failed: {:?} — marking child zombie", e);
            stub.exit_code.store(u64::MAX, Ordering::Relaxed);
            stub.state.store(TaskState::Zombie, Ordering::Release);
            // Wake ready_waiter first (sys_wait_task_ready caller learns load failed)
            if let Some((waiter, cpu_id)) = stub.ready_waiter.lock().take() {
                wake_task(waiter, cpu_id);
            }
            if let Some((waiter, cpu_id)) = stub.exit_waiter.lock().take() {
                wake_task(waiter, cpu_id);
            } else {
                TASK_TABLE.lock().remove(&stub.id);
            }
            drop(stub);
        }
    }

    sys_exit(0)
}

/// Syscall: block until the target task is no longer in Loading state.
///
/// Returns 0 if the task is now Ready/Running (load succeeded),
/// 1 if the task was not found, 2 if the load failed (task is Zombie).
/// Returns immediately if the task is already past Loading.
pub fn sys_wait_task_ready(task_id: u64, _: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    loop {
        let target = match TASK_TABLE.lock().get(&TaskId::from_u64(task_id)).cloned() {
            Some(t) => t,
            None => return 1,
        };

        let state = target.state.load(Ordering::Acquire);
        if state != TaskState::Loading {
            return if state == TaskState::Zombie { 2 } else { 0 };
        }

        // Still Loading: sleep on ready_waiter until the loader wakes us.
        if let Some((self_task, cpu_id)) = current_task_and_cpu() {
            {
                let mut slot = target.ready_waiter.lock();
                // Set Sleeping BEFORE storing the waiter so that a concurrent
                // spawn_task_activate sees Sleeping when it takes the slot.
                self_task.state.store(TaskState::Sleeping, Ordering::Release);
                *slot = Some((self_task.clone(), cpu_id));
            }
            // Re-check: the loader may have finished between the first state
            // check and waiter registration. spawn_task_activate acquires the
            // same lock, so if it ran while we held it, it couldn't take the
            // waiter yet. If it ran after we released, it already woke us.
            if target.state.load(Ordering::Acquire) != TaskState::Loading {
                self_task.state.store(TaskState::Ready, Ordering::Release);
                *target.ready_waiter.lock() = None;
                continue;
            }
        }

        x86_64::instructions::interrupts::enable();
        x86_64::instructions::hlt();
        x86_64::instructions::interrupts::disable();
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

    let (parent_cr3, user_vmas, parent_prio, parent_id) = {
        let rq = cpu.run_queue.get().unwrap().lock();
        let task = match &rq.current_task {
            Some(t) if t.kind == TaskKind::User => t.clone(),
            _ => return 0,
        };
        let inner = task.inner.lock();
        let prio = Priority::from_u8(task.priority.load(Ordering::Relaxed));
        let id = task.id;
        (task.cr3.load(Ordering::Relaxed), inner.user_vmas.clone(), prio, id)
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

    let task = Task::new_thread(entry, stack_top, parent_cr3, user_cs, user_ss, user_vmas, arg, name, parent_prio, Some(parent_id));
    let id = task.id.to_u64();
    crate::task::global_scheduler::spawn_task(task);
    id
}

/// Syscall: lower the current task's scheduling priority (cannot raise).
///
/// Returns: 0 if priority was lowered, 1 if already at or below requested level.
pub fn sys_set_priority(requested: u64, _: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    let prio = Priority::from_u8(requested as u8) as u8;
    let cpu = get_local();
    let rq = cpu.run_queue.get().unwrap().lock();
    let task = match &rq.current_task {
        Some(t) => t.clone(),
        None => return 1,
    };
    drop(rq);
    task.priority
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
            if prio < cur { Some(prio) } else { None }
        })
        .map(|_| 0u64)
        .unwrap_or(1)
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

        // Target still alive: register as exit waiter and sleep.
        // Set Sleeping BEFORE storing the waiter so that a concurrent
        // sys_exit wake sees Sleeping when it takes the slot.
        if let Some((self_task, cpu_id)) = current_task_and_cpu() {
            {
                let mut slot = target.exit_waiter.lock();
                self_task.state.store(TaskState::Sleeping, Ordering::Release);
                *slot = Some((self_task.clone(), cpu_id));
            }
            // Re-check: target may have exited between the state check
            // and waiter registration.
            if target.state.load(Ordering::Acquire) == TaskState::Zombie {
                self_task.state.store(TaskState::Ready, Ordering::Release);
                *target.exit_waiter.lock() = None;
                continue;
            }
        }

        // Clear in_syscall so the timer handler saves kernel state (normal
        // path) instead of returning directly to user-mode (syscall-yield
        // path). This lets the retry loop resume after re-scheduling.
        get_local().in_syscall_handler.store(0, Ordering::Relaxed);
        x86_64::instructions::interrupts::enable();
        x86_64::instructions::hlt();
        x86_64::instructions::interrupts::disable();
    }
}
