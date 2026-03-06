use alloc::sync::Arc;
use core::sync::atomic::Ordering::{AcqRel, Relaxed, Release};
use spin::Mutex;

use crate::drivers::{keyboard, mouse};
use crate::ipc::{channel_has_message, EndpointRole, ENDPOINT_REGISTRY};
use crate::memory::cpu_local_data::{get_cpu, get_local, local_apic_id_of};
use crate::task::local_scheduler;
use crate::task::task::{Task, TaskState};
use crate::time::tsc;

use super::{current_task_and_cpu, validate_user_ptr};

const MAX_TIMEOUT_WAITERS: usize = 16;
const NONE_TW: Option<(u64, Arc<Task>, u32)> = None;
static TIMEOUT_WAITERS: Mutex<[Option<(u64, Arc<Task>, u32)>; MAX_TIMEOUT_WAITERS]>
    = Mutex::new([NONE_TW; MAX_TIMEOUT_WAITERS]);

fn add_timeout_waiter(deadline: u64, task: Arc<Task>, cpu_id: u32) {
    let mut slots = TIMEOUT_WAITERS.lock();
    for slot in slots.iter_mut() {
        if slot.is_none() {
            *slot = Some((deadline, task, cpu_id));
            return;
        }
    }
    // 16 slots full — silently drop (won't happen in practice)
}

fn remove_timeout_waiter(task: &Arc<Task>) {
    let mut slots = TIMEOUT_WAITERS.lock();
    for slot in slots.iter_mut() {
        if let Some((_, t, _)) = slot {
            if Arc::ptr_eq(t, task) {
                *slot = None;
                return;
            }
        }
    }
}

/// Called by `timer_interrupt_handler_inner` every ~1 ms.
/// Wakes any task whose TSC deadline has expired.
pub fn check_timeout_waiters() {
    let now = tsc::value();
    loop {
        let entry = {
            let mut slots = TIMEOUT_WAITERS.lock();
            let pos = slots.iter().position(|s| {
                s.as_ref().map(|(d, _, _)| now >= *d).unwrap_or(false)
            });
            pos.and_then(|i| slots[i].take())
        };
        let (_, task, cpu_id) = match entry { Some(e) => e, None => break };
        if task.state.compare_exchange(TaskState::Sleeping, TaskState::Ready, AcqRel, Relaxed).is_ok() {
            let local_id = get_local().kernel_id;
            if cpu_id != local_id {
                local_scheduler::add(get_cpu(cpu_id), task);
                let apic_id = local_apic_id_of(cpu_id);
                crate::apic::send_fixed_ipi(apic_id, u8::from(crate::interrupt::InterruptVector::Reschedule));
            } else {
                // Same CPU: only add to the run queue if the task is NOT the current
                // task. If it IS (sleeping in hlt), schedule_from_interrupt will see
                // state=Ready and re-queue it exactly once. Adding here would create a
                // duplicate entry that accumulates over time → OOM.
                let is_current = {
                    let rq = get_cpu(cpu_id).run_queue.get().unwrap().lock();
                    rq.current_task.as_ref().map(|t| Arc::ptr_eq(t, &task)).unwrap_or(false)
                };
                if !is_current {
                    local_scheduler::add(get_cpu(cpu_id), task);
                }
            }
        }
    }
}

/// Syscall: block until any watched event source has data (or timeout expires).
///
/// Arguments:
///   channels_ptr   – userspace pointer to array of recv endpoint IDs
///   channel_count  – number of entries (clamped to 64)
///   flags          – WAIT_KEYBOARD (1) | WAIT_MOUSE (2)
///   timeout_ms     – 0 = infinite; non-zero = wake after this many ms
///
/// Returns: 0 = event available, 1 = timed out, 2 = invalid args
pub fn sys_wait_for_event(
    channels_ptr: u64,
    channel_count: u64,
    flags: u64,
    timeout_ms: u64,
    _: u64,
    _: u64,
) -> u64 {
    const MAX_CHANNELS: usize = 64;
    const RESULT_EVENT: u64 = 0;
    const RESULT_TIMEOUT: u64 = 1;
    const RESULT_INVALID: u64 = 2;

    let count = channel_count.min(MAX_CHANNELS as u64) as usize;

    // Validate and copy channel IDs from userspace
    let mut ep_ids = [0u64; MAX_CHANNELS];
    if count > 0 {
        if !validate_user_ptr(channels_ptr, (count as u64) * 8) {
            return RESULT_INVALID;
        }
        unsafe {
            core::ptr::copy_nonoverlapping(
                channels_ptr as *const u64,
                ep_ids.as_mut_ptr(),
                count,
            );
        }
    }

    let wait_keyboard = flags & kernel_api_types::WAIT_KEYBOARD as u64 != 0;
    let wait_mouse    = flags & kernel_api_types::WAIT_MOUSE    as u64 != 0;

    // Compute absolute TSC deadline (0 = infinite)
    let deadline_tsc: u64 = if timeout_ms == 0 {
        0
    } else {
        let hz = tsc::TSC_HZ.load(Relaxed); // ticks per ms
        tsc::value().saturating_add(timeout_ms.saturating_mul(hz))
    };

    loop {
        // 1. Non-blocking check: any channel has a queued message?
        for i in 0..count {
            if channel_has_message(ep_ids[i]) {
                return RESULT_EVENT;
            }
        }

        // 2. Mouse available?
        if wait_mouse && mouse::has_mouse() {
            return RESULT_EVENT;
        }

        // 3. Keyboard available?
        if wait_keyboard && keyboard::has_key() {
            return RESULT_EVENT;
        }

        // 4. Timeout expired?
        if deadline_tsc != 0 && tsc::value() >= deadline_tsc {
            return RESULT_TIMEOUT;
        }

        // Nothing ready — go to sleep.
        let (task, cpu_id) = match current_task_and_cpu() {
            Some(t) => t,
            None => return RESULT_EVENT, // shouldn't happen (kernel task)
        };

        // Pre-set CpuContext.rax so the task sees a valid result on spurious wakeup
        let ctx_ptr = get_local().current_context_ptr.load(Relaxed);
        if !ctx_ptr.is_null() {
            unsafe { (*ctx_ptr).rax = RESULT_EVENT; }
        }

        // Register in each channel's event_waiter
        for i in 0..count {
            let channel = {
                let registry = ENDPOINT_REGISTRY.lock();
                registry.get(&ep_ids[i])
                    .filter(|ep| ep.role == EndpointRole::Recv)
                    .map(|ep| ep.channel.clone())
            };
            if let Some(ch) = channel {
                *ch.event_waiter.lock() = Some((task.clone(), cpu_id));
            }
        }

        // Register mouse waiter
        if wait_mouse {
            *mouse::MOUSE_WAITER.lock() = Some((task.clone(), cpu_id));
        }

        // Register keyboard waiter
        if wait_keyboard {
            *keyboard::KEYBOARD_EVENT_WAITER.lock() = Some((task.clone(), cpu_id));
        }

        // Register timeout waiter
        if deadline_tsc != 0 {
            add_timeout_waiter(deadline_tsc, task.clone(), cpu_id);
        }

        // Mark sleeping — any ISR that fires after this point will CAS successfully
        task.state.store(TaskState::Sleeping, Release);

        // Enable interrupts, halt (resumes when any event ISR fires), then disable
        x86_64::instructions::interrupts::enable();
        x86_64::instructions::hlt();
        x86_64::instructions::interrupts::disable();

        // Clear our timeout registration if we woke due to a different event
        if deadline_tsc != 0 {
            remove_timeout_waiter(&task);
        }

        // Loop back: check what woke us and return, or re-register and sleep again
    }
}
