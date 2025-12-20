use crate::hlt_loop;
use crate::memory::cpu_local_data::{CpuLocalData, get_local};
use crate::task::global_scheduler::TASK_TABLE;
use crate::task::process::{CpuContext, ThreadId, ThreadState};
use alloc::collections::VecDeque;
use core::arch::naked_asm;
use core::sync::atomic::Ordering;

pub struct RunQueue {
    pub current: Option<ThreadId>,
    pub ready: VecDeque<ThreadId>,
}

pub fn schedule(cpu: &CpuLocalData) {
    let mut rq = cpu.run_queue.get().unwrap().lock();

    if let Some(current_id) = rq.current.take() {
        let tasks = TASK_TABLE.lock();
        if let Some(current) = tasks.get(&current_id) {
            // set state to READY
            current.state.store(ThreadState::Ready, Ordering::Relaxed);

            // push back into ready queue
            rq.ready.push_back(current_id);
        }
    }

    //pick the next task from ready queue
    if let Some(next_id) = rq.ready.pop_front() {
        let tasks = TASK_TABLE.lock();
        if let Some(next) = tasks.get(&next_id) {
            // set state to RUNNING
            next.state.store(ThreadState::Running, Ordering::Relaxed);

            // set as current
            rq.current = Some(next_id);

            //perform context switch
            unsafe {
                context_switch_to_task(&next.context);
            }
        }
    } else {
        // no task to run → run idle task or halt
        rq.current = None;
        log::info!("Run out of tasks to run.");
        hlt_loop();
    }
}

pub fn add(cpu: &CpuLocalData, task_id: ThreadId) {
    let mut rq = cpu.run_queue.get().unwrap().lock();

    let tasks = TASK_TABLE.lock();
    if let Some(task) = tasks.get(&task_id) {
        task.state.store(ThreadState::Ready, Ordering::Relaxed);

        rq.ready.push_back(task_id);
    } else {
        panic!("Task ID {:?} not found in TASK_TABLE", task_id);
    }
}

pub unsafe fn context_switch_to_task(new: &CpuContext) {
    let cpu = get_local(); // get CPU-local data
    if let Some(current_id) = cpu.run_queue.get().unwrap().lock().current {
        let tasks = TASK_TABLE.lock();
        if let Some(current) = tasks.get(&current_id) {
            unsafe {
                context_switch(&current.context as *const _ as *mut _, new as *const _);
            }
        }
    } else {
        // No current task, just jump to new
        unsafe {
            context_switch(core::ptr::null_mut(), new as *const _);
        }
    }
}

#[unsafe(naked)]
pub unsafe extern "C" fn context_switch(old: *mut CpuContext, new: *const CpuContext) {
    naked_asm!(
        // Save callee-saved registers into old
        "mov [rdi + 0x00], r15",
        "mov [rdi + 0x08], r14",
        "mov [rdi + 0x10], r13",
        "mov [rdi + 0x18], r12",
        "mov [rdi + 0x20], rbx",
        "mov [rdi + 0x28], rbp",
        // Save rsp, rip, rflags
        "mov [rdi + 0x30], rsp",
        "pushfq",
        "pop [rdi + 0x38]",
        "lea rax, [rip + 2f]",
        "mov [rdi + 0x40], rax",
        // Load new task registers
        "mov r15, [rsi + 0x00]",
        "mov r14, [rsi + 0x08]",
        "mov r13, [rsi + 0x10]",
        "mov r12, [rsi + 0x18]",
        "mov rbx, [rsi + 0x20]",
        "mov rbp, [rsi + 0x28]",
        "mov rsp, [rsi + 0x30]",
        "push [rsi + 0x38]", // rflags
        "popfq",
        "jmp [rsi + 0x40]", // rip
        // label for next instruction after returning
        "2:"
    );
}

/// # Safety
/// Stack must be valid
#[unsafe(naked)]
pub unsafe extern "sysv64" fn switch_to(new_rsp: u64, f: extern "sysv64" fn() -> !) {
    naked_asm!(
        "
        mov rsp, rdi
        call rsi
        "
    );
}

/// Safety: cpu_init must be called before
pub fn init_run_queue() {
    let cpu = get_local();

    cpu.run_queue.call_once(|| {
        spin::Mutex::new(RunQueue {
            current: None,
            ready: VecDeque::new(),
        })
    });
}
