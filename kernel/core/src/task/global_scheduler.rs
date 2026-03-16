use crate::interrupt::InterruptVector;
use crate::memory::cpu_local_data::{cpus_count, get_cpu, get_local, local_apic_id_of, try_get_ready_cpu};
use crate::task::task::{Task, TaskId, TaskKind, TaskState};
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;
use spin::Mutex;
use x86_64::instructions::interrupts;

pub static TASK_TABLE: Mutex<BTreeMap<TaskId, Arc<Task>>> = Mutex::new(BTreeMap::new());

/// Insert a task into TASK_TABLE without adding it to any run queue.
///
/// Used for Loading stubs: the task is visible to sys_waitpid and lookups
/// immediately, but does not run until `spawn_task_activate` is called.
pub fn preregister_task(arc: Arc<Task>) {
    interrupts::without_interrupts(|| {
        let mut table = TASK_TABLE.lock();
        if table.insert(arc.id, arc).is_some() {
            panic!("task ID collision in preregister_task");
        }
    });
}

/// Move a pre-registered Loading task to Ready and add it to a run queue.
///
/// Called by the kernel loader task after `fill_loading_task` succeeds.
/// Picks the least-loaded CPU using the same logic as `spawn_task`.
pub fn spawn_task_activate(arc: Arc<Task>) {
    let task_id = arc.id;
    let target_id = interrupts::without_interrupts(|| {
        // Set state Ready FIRST — the waiter's re-check must see Ready, not Loading.
        // Release ordering ensures cr3 (written with Release in fill_loading_task)
        // is visible to any Acquire load that observes this state transition.
        arc.state.store(TaskState::Ready, Ordering::Release);

        // Wake any task sleeping in sys_wait_task_ready.
        if let Some((waiter, cpu_id)) = arc.ready_waiter.lock().take() {
            waiter.state.store(TaskState::Ready, Ordering::Release);
            crate::task::local_scheduler::add(get_cpu(cpu_id), waiter);
            let local_id = get_local().kernel_id;
            if cpu_id != local_id {
                let apic_id = local_apic_id_of(cpu_id);
                crate::apic::send_fixed_ipi(apic_id, u8::from(InterruptVector::Reschedule));
            }
        }

        let total = cpus_count();
        let (target_id, target_cpu) = (0..total)
            .filter_map(|id| try_get_ready_cpu(id as u32).map(|cpu| (id, cpu)))
            .min_by_key(|(_, cpu)| cpu.ready_count.load(Ordering::Relaxed))
            .unwrap_or_else(|| {
                let local = get_local();
                (local.kernel_id as usize, local)
            });
        crate::task::local_scheduler::add(target_cpu, arc);

        let local = get_local();
        if target_id as u32 != local.kernel_id {
            let apic_id = local_apic_id_of(target_id as u32);
            crate::apic::send_fixed_ipi(apic_id, u8::from(InterruptVector::Reschedule));
        }

        target_id
    });

    log::info!(
        "Task {:?} activated on CPU {} and pushed to ready queue",
        task_id,
        target_id
    );
}

pub fn spawn_task(task: Task) {
    let task_id = task.id;
    let target_id = interrupts::without_interrupts(|| {
        task.state.store(TaskState::Ready, Ordering::Relaxed);
        let arc_task = Arc::new(task);

        // Insert into the global TASK_TABLE (for future lookups/kill/waitpid)
        let mut tasks = TASK_TABLE.lock();
        if tasks.insert(task_id, arc_task.clone()).is_some() {
            panic!("Task with the same ID already exists");
        }
        drop(tasks);

        // Least-loaded dispatch: pick the ready CPU with the fewest queued tasks.
        // This avoids sending IPIs to idle CPUs that have no work — a round-robin
        // counter would wake every CPU in turn even when only one has tasks.
        let total = cpus_count();
        let (target_id, target_cpu) = (0..total)
            .filter_map(|id| try_get_ready_cpu(id as u32).map(|cpu| (id, cpu)))
            .min_by_key(|(_, cpu)| cpu.ready_count.load(Ordering::Relaxed))
            .unwrap_or_else(|| {
                let local = get_local();
                (local.kernel_id as usize, local)
            });
        crate::task::local_scheduler::add(target_cpu, arc_task);

        // If target is a different CPU, send reschedule IPI to wake it from hlt
        let local = get_local();
        if target_id as u32 != local.kernel_id {
            let apic_id = local_apic_id_of(target_id as u32);
            crate::apic::send_fixed_ipi(apic_id, u8::from(InterruptVector::Reschedule));
        }

        target_id
    });

    log::info!(
        "Task {:?} scheduled on CPU {} and pushed to ready queue",
        task_id,
        target_id
    );
}

/// Spawn a task pinned to the calling CPU's run queue.
///
/// Unlike `spawn_task`, this bypasses round-robin dispatch and always places
/// the task on the local CPU. Used for idle tasks so each CPU gets its own.
pub fn spawn_local_task(task: Task) {
    let task_id = task.id;
    let cpu = get_local();
    interrupts::without_interrupts(|| {
        task.state.store(TaskState::Ready, Ordering::Relaxed);
        let arc_task = Arc::new(task);

        let mut tasks = TASK_TABLE.lock();
        if tasks.insert(task_id, arc_task.clone()).is_some() {
            panic!("Task with the same ID already exists");
        }
        drop(tasks);

        crate::task::local_scheduler::add(cpu, arc_task);
    });

    log::info!(
        "Task {:?} pinned to local CPU {}",
        task_id,
        cpu.kernel_id
    );
}

/// Remove and drop all zombie kernel tasks from TASK_TABLE.
///
/// Kernel tasks that call `sys_exit` are left in TASK_TABLE as Zombies (rather than
/// being removed in the ISR) because `GuardedStack::drop` calls the global allocator,
/// which may be held by the preempted task — calling it from the timer handler would
/// deadlock. This function must be called from task context (e.g., the idle task)
/// where allocator calls are safe.
///
/// The entire operation runs with interrupts disabled to prevent a preemption
/// deadlock: `GuardedStack::drop` acquires `physical_memory` and `virtual_memory`
/// spin locks. If the timer preempts while those locks are held, the scheduler
/// can switch to a task whose syscall handler also needs `physical_memory`
/// (e.g., `sys_transfer_display`), which spins forever because the lock holder
/// (this preempted idle task) cannot run on the same CPU.
///
/// Steps:
///   1. Collect zombie kernel task IDs under a short TASK_TABLE lock.
///   2. Remove them (second short lock), collecting the Arcs.
///   3. Drop the Arcs **outside** the TASK_TABLE lock so GuardedStack::drop
///      cannot create a lock-ordering cycle with code that holds physical_memory
///      and then tries to acquire TASK_TABLE.
pub fn reap_zombie_kernel_tasks() {
    interrupts::without_interrupts(|| {
        let zombie_ids: Vec<TaskId> = {
            let table = TASK_TABLE.lock();
            table
                .iter()
                .filter(|(_, arc)| {
                    arc.kind == TaskKind::Kernel
                        && arc.state.load(Ordering::Relaxed) == TaskState::Zombie
                })
                .map(|(id, _)| *id)
                .collect()
        };

        if zombie_ids.is_empty() {
            return;
        }

        let to_drop: Vec<Arc<Task>> = {
            let mut table = TASK_TABLE.lock();
            zombie_ids
                .into_iter()
                .filter_map(|id| table.remove(&id))
                .collect()
        };

        // Drop outside TASK_TABLE lock: GuardedStack::drop → physical_memory lock.
        drop(to_drop);
    });
}
