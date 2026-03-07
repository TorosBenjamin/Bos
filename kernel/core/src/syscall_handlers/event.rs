use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::AtomicUsize;
use core::sync::atomic::Ordering::{AcqRel, Relaxed, Release};
use spin::Mutex;

use crate::drivers::{keyboard, mouse};
use crate::ipc::{channel_has_message, EndpointRole, ENDPOINT_REGISTRY};
use crate::memory::cpu_local_data::{get_cpu, get_local, local_apic_id_of};
use crate::task::local_scheduler;
use crate::task::task::{Task, TaskState};
use crate::time::tsc;

use super::{current_task_and_cpu, validate_user_ptr};

/// Tasks sleeping in `sys_wait_for_event` with a finite timeout.
/// Each entry: (tsc_deadline, task_arc, cpu_id).
///
/// Uses a Vec so there is no hard limit on concurrent timeout waiters.
/// The accompanying atomic counter lets `check_timeout_waiters` skip the
/// lock entirely on every timer tick when the list is empty (common case).
static TIMEOUT_WAITERS: Mutex<Vec<(u64, Arc<Task>, u32)>> = Mutex::new(Vec::new());
static TIMEOUT_WAITER_COUNT: AtomicUsize = AtomicUsize::new(0);

fn add_timeout_waiter(deadline: u64, task: Arc<Task>, cpu_id: u32) {
    let mut v = TIMEOUT_WAITERS.lock();
    // Keep the vec sorted ascending by deadline so check_timeout_waiters can
    // stop at the first entry whose deadline has not yet arrived.
    let idx = v.partition_point(|(d, _, _)| *d <= deadline);
    v.insert(idx, (deadline, task, cpu_id));
    TIMEOUT_WAITER_COUNT.store(v.len(), Relaxed);
}

fn remove_timeout_waiter(task: &Arc<Task>) {
    let mut v = TIMEOUT_WAITERS.lock();
    if let Some(pos) = v.iter().position(|(_, t, _)| Arc::ptr_eq(t, task)) {
        v.remove(pos); // preserve sort order (swap_remove would break it)
        TIMEOUT_WAITER_COUNT.store(v.len(), Relaxed);
    }
}

/// Called by `timer_interrupt_handler_inner` every ~1 ms.
/// Wakes any task whose TSC deadline has expired.
pub fn check_timeout_waiters() {
    // Fast path: avoid lock acquisition when nothing is sleeping with a timeout.
    // The count is updated under the lock; reading it Relaxed here means we might
    // see a stale zero and skip one tick — at worst we add one timer period of
    // latency, which is well within the timeout precision we guarantee.
    if TIMEOUT_WAITER_COUNT.load(Relaxed) == 0 {
        return;
    }

    let now = tsc::value();
    loop {
        let entry = {
            let mut v = TIMEOUT_WAITERS.lock();
            // Vec is sorted ascending by deadline; the front entry is the earliest.
            // If it hasn't expired yet, nothing else has either — stop immediately.
            if v.first().map(|(d, _, _)| now < *d).unwrap_or(true) {
                break;
            }
            let e = v.remove(0);
            TIMEOUT_WAITER_COUNT.store(v.len(), Relaxed);
            e
        };
        let (_, task, cpu_id) = entry;

        if task.state.compare_exchange(TaskState::Sleeping, TaskState::Ready, AcqRel, Relaxed).is_ok() {
            let local_id = get_local().kernel_id;
            if cpu_id != local_id {
                local_scheduler::add(get_cpu(cpu_id), task);
                let apic_id = local_apic_id_of(cpu_id);
                crate::apic::send_fixed_ipi(apic_id, u8::from(crate::interrupt::InterruptVector::Reschedule));
            } else {
                // Same CPU: only add to the run queue if the task is NOT the current
                // task. If it IS (sleeping in hlt), schedule_from_interrupt will see
                // state=Ready and re-queue it exactly once.
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
///
/// ## Wakeup protocol
///
/// The waiter-registration and sleep sequence is ordered carefully to avoid
/// a TOCTOU race between "nothing ready" and "going to sleep":
///
///   1. Poll all sources — return immediately if anything is ready.
///   2. **Register** in all event-waiter slots (channels, mouse, keyboard, timeout).
///   3. **Re-poll** all sources — catches events that arrived between step 1 and 2.
///      If ready: remove timeout waiter and return. Stale channel/mouse/keyboard
///      waiter entries are self-cleaning: the next ISR that fires will call
///      `try_wake_slot`, the CAS(Sleeping→Ready) fails (state≠Sleeping), and the
///      Arc is dropped harmlessly.
///   4. Set state=Sleeping, enable interrupts, hlt.
///
/// Any ISR that fires after step 2 but before step 4 will fail CAS because the
/// task state is still Running, not Sleeping. Because event data is buffered
/// (channel messages, keyboard/mouse ring buffers), the re-poll in step 3 catches
/// it. The remaining window — between step 3 and step 4 — is a handful of
/// instructions: if an event arrives there, the task sleeps until the next LAPIC
/// timer tick (~1 ms), wakes, loops back to step 1, and finds the buffered data.
pub fn sys_wait_for_event(
    channels_ptr: u64,
    channel_count: u64,
    flags: u64,
    timeout_ms: u64,
    _: u64,
    _: u64,
) -> u64 {
    const MAX_CHANNELS: usize = 64;
    const RESULT_EVENT:   u64 = 0;
    const RESULT_TIMEOUT: u64 = 1;
    const RESULT_INVALID: u64 = 2;

    let count = channel_count.min(MAX_CHANNELS as u64) as usize;

    // Validate and copy channel IDs from userspace.
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

    // Compute absolute TSC deadline (0 = infinite).
    let deadline_tsc: u64 = if timeout_ms == 0 {
        0
    } else {
        let hz = tsc::TSC_TICKS_PER_MS.load(Relaxed);
        tsc::value().saturating_add(timeout_ms.saturating_mul(hz))
    };

    // Resolve task identity once — it never changes across loop iterations.
    let (task, cpu_id) = match current_task_and_cpu() {
        Some(t) => t,
        None    => return RESULT_EVENT, // shouldn't happen (kernel task)
    };

    // Pre-set the fallback return value in the saved register context so that
    // a spurious wakeup (e.g. stale waiter fired by a late ISR) returns EVENT
    // rather than whatever happened to be in RAX.
    let ctx_ptr = get_local().current_context_ptr.load(Relaxed);
    if !ctx_ptr.is_null() {
        unsafe { (*ctx_ptr).rax = RESULT_EVENT; }
    }

    loop {
        // ── Step 1: non-blocking poll ─────────────────────────────────────────
        for i in 0..count {
            if channel_has_message(ep_ids[i]) { return RESULT_EVENT; }
        }
        if wait_mouse    && mouse::has_mouse()  { return RESULT_EVENT; }
        if wait_keyboard && keyboard::has_key() { return RESULT_EVENT; }
        if deadline_tsc != 0 && tsc::value() >= deadline_tsc { return RESULT_TIMEOUT; }

        // ── Step 2: register in all event-waiter slots ────────────────────────
        // Done BEFORE the re-poll so that any event arriving from this point
        // onward either (a) is caught by the re-poll below, or (b) wakes us
        // via ISR after we set state=Sleeping.
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
        if wait_mouse    { *mouse::MOUSE_WAITER.lock()             = Some((task.clone(), cpu_id)); }
        if wait_keyboard { *keyboard::KEYBOARD_EVENT_WAITER.lock() = Some((task.clone(), cpu_id)); }
        if deadline_tsc != 0 { add_timeout_waiter(deadline_tsc, task.clone(), cpu_id); }

        // ── Step 3: re-poll after registration ───────────────────────────────
        // Catches events that arrived in the window between step 1 and step 2.
        // Stale waiter entries left in channel/mouse/keyboard slots are
        // self-cleaning: the next try_wake_slot call fails CAS (state≠Sleeping)
        // and discards the Arc. We do need to explicitly remove the timeout entry.
        let timed_out   = deadline_tsc != 0 && tsc::value() >= deadline_tsc;
        let event_ready = (0..count).any(|i| channel_has_message(ep_ids[i]))
            || (wait_mouse    && mouse::has_mouse())
            || (wait_keyboard && keyboard::has_key());

        if event_ready || timed_out {
            if deadline_tsc != 0 { remove_timeout_waiter(&task); }
            return if timed_out { RESULT_TIMEOUT } else { RESULT_EVENT };
        }

        // ── Step 4: sleep ─────────────────────────────────────────────────────
        task.state.store(TaskState::Sleeping, Release);
        x86_64::instructions::interrupts::enable();
        x86_64::instructions::hlt();
        x86_64::instructions::interrupts::disable();

        // Clean up the timeout slot in case we were woken by a different source.
        if deadline_tsc != 0 { remove_timeout_waiter(&task); }

        // Loop back to step 1: re-check all sources to determine the return value
        // or go back to sleep if this was a spurious wakeup.
    }
}
