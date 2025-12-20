use crate::memory::guarded_stack::{GuardedStack, StackId, StackType, NORMAL_STACK_SIZE};
use alloc::sync::Arc;
use atomic_enum::atomic_enum;
use core::sync::atomic::{AtomicU64, Ordering};
use ez_paging::ManagedL4PageTable;
use x86_64::registers::segmentation::CS;
use crate::memory::cpu_local_data::get_local;
use crate::memory::vaddr_allocator::VirtualMemoryAllocator;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ThreadId(u64);

impl ThreadId {
    fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        ThreadId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }
}

#[atomic_enum]
pub enum ThreadState {
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
    pub rsp: u64,
    pub rflags: u64,
    pub rip: u64,
}

pub type ThreadFn = fn() -> !;

pub struct KernelThread {
    pub id: ThreadId,
    pub state: AtomicThreadState,

    // CPU context saved during preemption
    pub context: CpuContext,

    // Stacks
    pub kernel_stack: GuardedStack,
}

impl KernelThread {
    pub fn new(func: ThreadFn) -> Arc<Self> {
        let kernel_stack = GuardedStack::new_kernel(
            NORMAL_STACK_SIZE,
            StackId {
                _type: StackType::Normal,
                cpu_id: get_local().kernel_id,
            }
        );
        let rsp = kernel_stack.top().as_u64();
        let context = CpuContext {
            r15: 0,
            r14: 0,
            r13: 0,
            r12: 0,
            rbx: 0,
            rbp: 0,
            rsp,                  // top of kernel stack
            rflags: 0x202,        // interrupt enable
            rip: func as u64,     // start executing the function

        };

        Arc::new(
            KernelThread {
                id: ThreadId::new(),
                state: AtomicThreadState::new(ThreadState::Ready),
                context,
                kernel_stack,
            }
        )
    }
}
