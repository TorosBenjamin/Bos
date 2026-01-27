use crate::memory::guarded_stack::{GuardedStack, StackId, StackType, NORMAL_STACK_SIZE};
use atomic_enum::atomic_enum;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::mutex::Mutex;
use crate::memory::cpu_local_data::get_local;
use crate::task::context;
use x86_64::instructions::segmentation::{CS, Segment};

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

        // Prepare a synthetic interrupt stack frame for iretq.
        let mut sp = stack.top().as_u64();
        sp &= !0xF;

        // iretq frame
        sp -= 8; // SS
        unsafe { (sp as *mut u64).write(0) };
        sp -= 8; // RSP
        unsafe { (sp as *mut u64).write(stack.top().as_u64()) };
        sp -= 8; // RFLAGS
        unsafe { (sp as *mut u64).write(0x202) };
        sp -= 8; // CS
        let cs = CS::get_reg().0 as u64;
        unsafe { (sp as *mut u64).write(cs) };
        sp -= 8; // RIP
        unsafe { (sp as *mut u64).write(task_trampoline as u64) };

        // Registers pushed by timer_interrupt_handler
        sp -= core::mem::size_of::<Registers>() as u64;
        let regs = sp as *mut Registers;
        unsafe {
            regs.write(Registers {
                r15: entry as usize,
                r14: 0,
                r13: 0,
                r12: 0,
                r11: 0,
                r10: 0,
                r9: 0,
                r8: 0,
                rdi: 0,
                rsi: 0,
                rbp: 0,
                rbx: 0,
                rdx: 0,
                rcx: 0,
                rax: 0,
            });
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

    pub fn set_state(&self, state: TaskState) {
        self.state.store(state, Ordering::Relaxed);
    }

    pub fn is_runnable(&self) -> bool {
        self.run_state() == TaskState::Ready
    }
}

/// Loads the actual function from the first register
#[unsafe(no_mangle)]
#[unsafe(naked)]
extern "C" fn task_trampoline() -> ! {
    core::arch::naked_asm!(
    "sti",           // ENABLE INTERRUPTS
    "mov rax, r15",  // load function pointer
    "jmp rax",        // jump to it
    )
}

#[repr(C)]
struct Registers {
    r15: usize,
    r14: usize,
    r13: usize,
    r12: usize,
    r11: usize,
    r10: usize,
    r9: usize,
    r8: usize,
    rdi: usize,
    rsi: usize,
    rbp: usize,
    rbx: usize,
    rdx: usize,
    rcx: usize,
    rax: usize,
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
