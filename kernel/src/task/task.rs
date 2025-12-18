use crate::memory::guarded_stack::GuardedStack;
use alloc::sync::Arc;
use atomic_enum::atomic_enum;
use core::sync::atomic::{AtomicU64, Ordering};
use ez_paging::ManagedL4PageTable;
use spin::Mutex;
use x86_64::VirtAddr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TaskId(u64);

impl TaskId {
    fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        TaskId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }
}

#[atomic_enum]
pub enum TaskState {
    Running,
    Ready,
    Blocked,
    Sleeping,
    Zombie,
}

#[repr(C)]
pub struct CpuContext {
    // callee-saved registers
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub rbx: u64,
    pub rbp: u64,

    // return frame
    pub rip: u64,
    pub rsp: u64,
    pub rflags: u64,

    pub cs: u64,
    pub ss: u64,
}

pub type TaskFn = fn() -> !;

pub struct Task {
    pub id: TaskId,
    pub state: AtomicTaskState,

    // CPU context saved during preemption
    pub context: CpuContext,

    // Stacks
    pub kernel_stack: GuardedStack,
    pub user_stack: GuardedStack,

    // Memory
    pub address_space: Arc<ManagedL4PageTable>,
}

impl Task {
    pub fn new(func: TaskFn, kernel_stack: GuardedStack) -> Arc<Self> {
        todo!();
        /*
        let rsp = kernel_stack.top().as_u64();
        let context = CpuContext {
            r15: 0,
            r14: 0,
            r13: 0,
            r12: 0,
            rbx: 0,
            rbp: 0,
            rip: func as u64,     // start executing the function
            rsp,                  // top of kernel stack
            rflags: 0x202,        // interrupt enable
            cs: ,
            ss: ,
        };

        Arc::new(Task {
            id: TaskId::new(),
            state: AtomicTaskState::new(TaskState::Ready),
            context,
            kernel_stack,
            user_stack: GuardedStack::new_kernel()
        })
        */
    }
}
