use crate::memory::guarded_stack::{GuardedStack, StackId, StackType, NORMAL_STACK_SIZE};
use atomic_enum::atomic_enum;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::mutex::Mutex;
use crate::memory::cpu_local_data::get_local;
use x86_64::instructions::segmentation::{CS, SS, Segment};

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

/// Initial stack frame for a new task, matching the layout that
/// `timer_interrupt_handler` expects to pop: 15 GPRs + iretq frame.
///
/// Fields are ordered from lowest address (where RSP points) to highest.
#[repr(C)]
struct InitialTaskFrame {
    // GPRs — popped by the timer handler (r15 first, rax last)
    r15: u64,
    r14: u64,
    r13: u64,
    r12: u64,
    r11: u64,
    r10: u64,
    r9: u64,
    r8: u64,
    rdi: u64,
    rsi: u64,
    rbp: u64,
    rbx: u64,
    rdx: u64,
    rcx: u64,
    rax: u64,
    // iretq frame
    rip: u64,
    cs: u64,
    rflags: u64,
    rsp: u64,
    ss: u64,
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

        let stack_top = stack.top().as_u64();

        // Place the initial frame at the top of the stack.
        // stack_top is page-aligned (thus 16-byte aligned).
        // InitialTaskFrame is 20 * 8 = 160 bytes, which is 16-byte aligned,
        // so frame_addr is also 16-byte aligned.
        let frame_size = core::mem::size_of::<InitialTaskFrame>() as u64;
        let frame_addr = stack_top - frame_size;
        let frame_ptr = frame_addr as *mut InitialTaskFrame;

        // Read current segment selectors so the iretq frame returns to kernel mode.
        let cs = CS::get_reg().0 as u64;
        let ss = SS::get_reg().0 as u64;

        unsafe {
            core::ptr::write(frame_ptr, InitialTaskFrame {
                r15: entry as u64, // trampoline reads entry fn from r15
                r14: 0, r13: 0, r12: 0, r11: 0, r10: 0, r9: 0, r8: 0,
                rdi: 0, rsi: 0, rbp: 0, rbx: 0, rdx: 0, rcx: 0, rax: 0,
                rip: task_trampoline as u64,
                cs,
                rflags: 0x200, // IF=1 (interrupts enabled on entry)
                rsp: stack_top, // after iretq, task uses full stack from the top
                ss,
            });
        }

        Task {
            inner: Mutex::new(TaskInner {
                rsp: frame_addr as usize,
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
        "mov rdi, r15",  // Move the function pointer to RDI
        "call rdi",      // Call the function
        "ud2",           // Should not return
    )
}
