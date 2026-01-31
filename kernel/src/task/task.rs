use crate::memory::guarded_stack::{GuardedStack, StackId, StackType, NORMAL_STACK_SIZE};
use atomic_enum::atomic_enum;
use core::sync::atomic::{AtomicU64, Ordering};
use ez_paging::ManagedL4PageTable;
use nodit::{Interval, NoditSet};
use spin::mutex::Mutex;
use crate::memory::cpu_local_data::get_local;
use x86_64::instructions::segmentation::{CS, SS, Segment};
use x86_64::registers::control::Cr3;

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

    pub fn to_u64(self) -> u64 {
        self.0
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

/// Whether a task runs in kernel mode or user mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    Kernel,
    User,
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

/// Parts of the task that can be modified after creation
pub struct TaskInner {
    pub rsp: usize,
    pub kernel_stack: GuardedStack,
    pub kernel_stack_top: u64,
    /// Owns the user-mode page table (keeps it alive). None for kernel tasks.
    pub user_page_table: Option<ManagedL4PageTable>,
    /// Tracks user-space virtual address allocations (ELF segments, stack, mmap).
    /// Empty for kernel tasks.
    pub user_vaddr_set: NoditSet<u64, Interval<u64>>,
}

pub struct Task {
    pub inner: Mutex<TaskInner>,
    pub id: TaskId,
    pub state: AtomicTaskState,
    pub kind: TaskKind,
    /// Physical address of the L4 page table for this task.
    /// For kernel tasks, this is the kernel CR3.
    pub cr3: u64,
}

impl Task {
    /// Create a new kernel-mode task.
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
        let frame_size = core::mem::size_of::<InitialTaskFrame>() as u64;
        let frame_addr = stack_top - frame_size;
        let frame_ptr = frame_addr as *mut InitialTaskFrame;

        // Read current segment selectors so the iretq frame returns to kernel mode.
        let cs = CS::get_reg().0 as u64;
        let ss = SS::get_reg().0 as u64;

        // Read current CR3 for kernel tasks
        let (cr3_frame, _) = Cr3::read();
        let cr3 = cr3_frame.start_address().as_u64();

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
                kernel_stack: stack,
                kernel_stack_top: stack_top,
                user_page_table: None,
                user_vaddr_set: NoditSet::default(),
            }),
            id: TaskId::new(),
            state: AtomicTaskState::new(TaskState::Initializing),
            kind: TaskKind::Kernel,
            cr3,
        }
    }

    /// Create a new user-mode task.
    ///
    /// - `entry_rip`: User-space entry point (ELF entry)
    /// - `user_rsp`: Top of the user-space stack
    /// - `page_table`: The user-mode page table (ownership transferred)
    /// - `cr3`: Physical address of the user page table's L4 frame
    /// - `user_cs`: User code segment selector
    /// - `user_ss`: User data segment selector
    pub fn new_user(
        entry_rip: u64,
        user_rsp: u64,
        page_table: ManagedL4PageTable,
        cr3: u64,
        user_cs: u16,
        user_ss: u16,
        user_vaddr_set: NoditSet<u64, Interval<u64>>,
        arg: u64,
    ) -> Self {
        // Allocate a kernel stack for this user task (used for interrupts/syscalls)
        let kernel_stack = GuardedStack::new_kernel(
            NORMAL_STACK_SIZE,
            StackId {
                _type: StackType::Normal,
                cpu_id: get_local().kernel_id,
            },
        );

        let kernel_stack_top = kernel_stack.top().as_u64();

        // Place InitialTaskFrame on the kernel stack with user CS/SS in iretq frame.
        // When the scheduler first switches to this task, it will pop the GPRs and iretq
        // into user mode.
        let frame_size = core::mem::size_of::<InitialTaskFrame>() as u64;
        let frame_addr = kernel_stack_top - frame_size;
        let frame_ptr = frame_addr as *mut InitialTaskFrame;

        unsafe {
            core::ptr::write(frame_ptr, InitialTaskFrame {
                // All GPRs zeroed for clean user register state
                r15: 0, r14: 0, r13: 0, r12: 0, r11: 0, r10: 0, r9: 0, r8: 0,
                rdi: arg, rsi: 0, rbp: 0, rbx: 0, rdx: 0, rcx: 0, rax: 0,
                rip: entry_rip,
                cs: user_cs as u64,
                rflags: 0x200, // IF=1 (interrupts enabled in user mode)
                rsp: user_rsp,
                ss: user_ss as u64,
            });
        }

        Task {
            inner: Mutex::new(TaskInner {
                rsp: frame_addr as usize,
                kernel_stack,
                kernel_stack_top,
                user_page_table: Some(page_table),
                user_vaddr_set,
            }),
            id: TaskId::new(),
            state: AtomicTaskState::new(TaskState::Initializing),
            kind: TaskKind::User,
            cr3,
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
