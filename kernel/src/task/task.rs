use crate::memory::guarded_stack::{GuardedStack, StackId, StackType, NORMAL_STACK_SIZE};
use atomic_enum::atomic_enum;
use core::sync::atomic::{AtomicU64, Ordering};
use nodit::{Interval, NoditSet};
use spin::mutex::Mutex;
use crate::memory::cpu_local_data::get_local;
use x86_64::instructions::segmentation::{CS, SS, Segment};
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::PhysFrame;

/// CPU context saved/restored on task switches.
/// Layout matches assembly expectations - DO NOT reorder fields.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CpuContext {
    // GPRs (offset 0-119, 15 registers * 8 bytes)
    pub r15: u64,  // offset 0
    pub r14: u64,  // offset 8
    pub r13: u64,  // offset 16
    pub r12: u64,  // offset 24
    pub r11: u64,  // offset 32
    pub r10: u64,  // offset 40
    pub r9: u64,   // offset 48
    pub r8: u64,   // offset 56
    pub rdi: u64,  // offset 64
    pub rsi: u64,  // offset 72
    pub rbp: u64,  // offset 80
    pub rbx: u64,  // offset 88
    pub rdx: u64,  // offset 96
    pub rcx: u64,  // offset 104
    pub rax: u64,  // offset 112
    // iretq frame (offset 120-159)
    pub rip: u64,    // offset 120
    pub cs: u64,     // offset 128
    pub rflags: u64, // offset 136
    pub rsp: u64,    // offset 144
    pub ss: u64,     // offset 152
}

// Offset constants for assembly access
pub const CTX_R15: usize = 0;
pub const CTX_R14: usize = 8;
pub const CTX_R13: usize = 16;
pub const CTX_R12: usize = 24;
pub const CTX_R11: usize = 32;
pub const CTX_R10: usize = 40;
pub const CTX_R9: usize = 48;
pub const CTX_R8: usize = 56;
pub const CTX_RDI: usize = 64;
pub const CTX_RSI: usize = 72;
pub const CTX_RBP: usize = 80;
pub const CTX_RBX: usize = 88;
pub const CTX_RDX: usize = 96;
pub const CTX_RCX: usize = 104;
pub const CTX_RAX: usize = 112;
pub const CTX_RIP: usize = 120;
pub const CTX_CS: usize = 128;
pub const CTX_RFLAGS: usize = 136;
pub const CTX_RSP: usize = 144;
pub const CTX_SS: usize = 152;

impl Default for CpuContext {
    fn default() -> Self {
        Self {
            r15: 0, r14: 0, r13: 0, r12: 0, r11: 0, r10: 0, r9: 0, r8: 0,
            rdi: 0, rsi: 0, rbp: 0, rbx: 0, rdx: 0, rcx: 0, rax: 0,
            rip: 0, cs: 0, rflags: 0, rsp: 0, ss: 0,
        }
    }
}

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

    pub fn from_u64(v: u64) -> Self {
        TaskId(v)
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

/// Parts of the task that can be modified after creation
pub struct TaskInner {
    pub context: CpuContext,
    pub kernel_stack: GuardedStack,
    pub kernel_stack_top: u64,
    /// Owns the user-mode page table (keeps it alive). None for kernel tasks.
    pub user_page_table: Option<PhysFrame>,
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

        // Read current segment selectors so the iretq frame returns to kernel mode.
        let cs = CS::get_reg().0 as u64;
        let ss = SS::get_reg().0 as u64;

        // Read current CR3 for kernel tasks
        let (cr3_frame, _) = Cr3::read();
        let cr3 = cr3_frame.start_address().as_u64();

        // Initialize context in the Task struct
        let context = CpuContext {
            r15: entry as u64, // trampoline reads entry fn from r15
            r14: 0, r13: 0, r12: 0, r11: 0, r10: 0, r9: 0, r8: 0,
            rdi: 0, rsi: 0, rbp: 0, rbx: 0, rdx: 0, rcx: 0, rax: 0,
            rip: task_trampoline as u64,
            cs,
            rflags: 0x200, // IF=1 (interrupts enabled on entry)
            rsp: stack_top, // after iretq, task uses full stack from the top
            ss,
        };

        Task {
            inner: Mutex::new(TaskInner {
                context,
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
        page_table: PhysFrame,
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

        // Initialize context in the Task struct (not on the stack)
        let context = CpuContext {
            // All GPRs zeroed for clean user register state
            r15: 0, r14: 0, r13: 0, r12: 0, r11: 0, r10: 0, r9: 0, r8: 0,
            rdi: arg, rsi: 0, rbp: 0, rbx: 0, rdx: 0, rcx: 0, rax: 0,
            rip: entry_rip,
            cs: user_cs as u64,
            rflags: 0x200, // IF=1 (interrupts enabled in user mode)
            rsp: user_rsp,
            ss: user_ss as u64,
        };

        Task {
            inner: Mutex::new(TaskInner {
                context,
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
