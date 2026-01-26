use crate::memory::guarded_stack::{GuardedStack, StackId, StackType, NORMAL_STACK_SIZE};
use alloc::sync::Arc;
use atomic_enum::atomic_enum;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::mutex::Mutex;
use crate::memory::cpu_local_data::get_local;
use crate::task::context;
use crate::task::context::Context;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TaskId(u64);

impl TaskId {
    fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        TaskId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }

    pub fn to_usize(self) -> usize {
        self.0 as usize
    }
}

#[atomic_enum]
#[derive(PartialEq)]
pub enum TaskState {
    Initializing,
    Running,
    Ready,
    Sleeping,
    Zombie,
}

/// Parts of the tasks that can be modified after creation
pub struct TaskInner {
    pub rsp: usize,
    pub stack: GuardedStack,
}

pub struct Task {
    pub inner: Mutex<TaskInner>,
    pub id: TaskId,
    pub state: AtomicTaskState,
}

impl Task {
    pub fn new(entry: fn() -> !) -> Self {
        let stack = GuardedStack::new_kernel(
            NORMAL_STACK_SIZE,
            StackId {
                _type: StackType::Normal,
                cpu_id: get_local().kernel_id,
            }
        );

        // Align stack
        let mut sp = stack.top().as_u64();
        sp -= core::mem::size_of::<Context>() as u64;

        let ctx = sp as *mut Context;

        // Set entry
        unsafe {
            ctx.write(Context::new(task_trampoline as usize));
            ctx.as_mut().unwrap().set_first_register(entry as usize);
        }

        Task {
            inner: Mutex::new(TaskInner {
                rsp: sp as usize,
                stack
            }),
            id: TaskId::new(),
            state: AtomicTaskState::new(TaskState::Initializing),
        }
    }

    pub fn run_state(&self) -> TaskState { self.state.load(Ordering::Relaxed) }

    pub fn is_runnable(&self) -> bool {
        self.run_state() == TaskState::Ready
    }
}

/// Loads the actual function from the first register
#[unsafe(no_mangle)]
#[unsafe(naked)]
extern "C" fn task_trampoline() -> ! {
    unsafe {
        core::arch::naked_asm!(
        "mov rax, r15",  // load function pointer
        "jmp rax",        // jump to it
        )
    }
}


/// Switch to the next task.
/// Safety: The next task must have a valid stack.
pub fn switch(prev: &Task, next: &Task) {
    let mut prev = prev.inner.lock();
    let next = next.inner.lock();

    unsafe {
        context::switch(
            &mut prev.rsp as *mut usize,
            next.rsp
        );
    }
}

pub fn switch_to_new(next: &Task) {
    let mut next = next.inner.lock();

    unsafe {
        context::switch_to_new(&mut next.rsp as *mut usize)
    }
}