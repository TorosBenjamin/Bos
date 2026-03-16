use crate::memory::guarded_stack::{GuardedStack, StackId, StackType, NORMAL_STACK_SIZE};
use crate::memory::MEMORY;
use crate::memory::physical_memory::MemoryType;
use alloc::sync::Arc;
use alloc::vec::Vec;
use atomic_enum::atomic_enum;
use core::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use kernel_api_types::Priority;
use nodit::{Interval, NoditMap};
use spin::mutex::Mutex;
use crate::memory::cpu_local_data::get_local;
use x86_64::instructions::segmentation::{CS, SS, Segment};
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::{PageTable, PageTableFlags, PhysFrame};

/// Whether a VMA's backing frames are pre-installed or filled on demand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VmaBacking {
    /// Frames were installed at region creation (ELF segments, shared bufs).
    /// A not-present fault inside this region is always a bug — kill the task.
    EagerlyMapped,
    /// Zero-fill on first access (anonymous mmap, user stack).
    Anonymous,
}

/// Metadata stored per virtual-memory region in the VMA map.
#[derive(Clone, Copy, Debug)]
pub struct VmaEntry {
    /// PTE flags used when installing a frame (PRESENT | USER_ACCESSIBLE | ...).
    pub flags: PageTableFlags,
    pub backing: VmaBacking,
}
use x86_64::{PhysAddr, VirtAddr};

/// CPU context saved/restored on task switches.
/// Layout matches assembly expectations - DO NOT reorder fields.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
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


static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TaskId(u64);

impl TaskId {
    fn new() -> Self {
        TaskId(NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed))
    }

    /// Pre-allocate a task ID without creating a Task struct.
    /// Used by sys_spawn to register a Loading stub before the loader task runs.
    pub fn alloc() -> Self {
        TaskId(NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed))
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
    Loading,       // stub in TASK_TABLE; ELF load in progress by a kernel loader task
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
    pub kernel_stack: Option<GuardedStack>,
    pub kernel_stack_top: u64,
    /// Owns the user-mode page table (keeps it alive). None for kernel tasks.
    pub user_page_table: Option<PhysFrame>,
    /// Per-region metadata for user-space virtual address allocations.
    /// Empty for kernel tasks.
    pub user_vmas: NoditMap<u64, Interval<u64>, VmaEntry>,
    /// IPC endpoint IDs owned by this task; closed on exit.
    pub owned_endpoints: Vec<u64>,
    /// Service names registered by this task; removed from the registry on exit.
    pub registered_services: Vec<[u8; 64]>,
}

/// Walk L4 entries 0..256 (user space) and free all page table frames and data frames.
/// All user frames are `UsedByUserMode`.
///
/// # Safety
/// `l4_phys` must be a valid physical address of a user page table L4 frame.
/// Must not be called while the page table is still active (CR3).
unsafe fn free_user_address_space(
    l4_phys: PhysAddr,
    phys_mem: &mut crate::memory::physical_memory::PhysicalMemory,
) {
    let hhdm = crate::memory::hhdm_offset::hhdm_offset().as_u64();

    let l4 = unsafe { &*VirtAddr::new(hhdm + l4_phys.as_u64()).as_ptr::<PageTable>() };
    for i in 0..256usize {
        let l4e = &l4[i];
        if !l4e.flags().contains(PageTableFlags::PRESENT) {
            continue;
        }
        let l3_phys = l4e.addr();
        let l3 = unsafe { &*VirtAddr::new(hhdm + l3_phys.as_u64()).as_ptr::<PageTable>() };
        for l3e in l3.iter() {
            if !l3e.flags().contains(PageTableFlags::PRESENT) {
                continue;
            }
            let l2_phys = l3e.addr();
            let l2 = unsafe { &*VirtAddr::new(hhdm + l2_phys.as_u64()).as_ptr::<PageTable>() };
            for l2e in l2.iter() {
                if !l2e.flags().contains(PageTableFlags::PRESENT) {
                    continue;
                }
                let l1_phys = l2e.addr();
                let l1 = unsafe { &*VirtAddr::new(hhdm + l1_phys.as_u64()).as_ptr::<PageTable>() };
                for l1e in l1.iter() {
                    if !l1e.flags().contains(PageTableFlags::PRESENT) {
                        continue;
                    }
                    let _ = phys_mem.free_frame(
                        PhysFrame::containing_address(l1e.addr()),
                        MemoryType::UsedByUserMode,
                    );
                }
                let _ = phys_mem.free_frame(
                    PhysFrame::containing_address(l1_phys),
                    MemoryType::UsedByUserMode,
                );
            }
            let _ = phys_mem.free_frame(
                PhysFrame::containing_address(l2_phys),
                MemoryType::UsedByUserMode,
            );
        }
        let _ = phys_mem.free_frame(
            PhysFrame::containing_address(l3_phys),
            MemoryType::UsedByUserMode,
        );
    }
    // Free the L4 frame itself
    let _ = phys_mem.free_frame(
        PhysFrame::containing_address(l4_phys),
        MemoryType::UsedByUserMode,
    );
}

impl Drop for TaskInner {
    fn drop(&mut self) {
        if let Some(l4_frame) = self.user_page_table.take() {
            let memory = MEMORY.get().unwrap();
            let mut phys_mem = memory.physical_memory.lock();
            unsafe { free_user_address_space(l4_frame.start_address(), &mut phys_mem); }
        }
        // kernel_stack's GuardedStack::drop runs automatically when the Option drops
    }
}

pub struct Task {
    pub inner: Mutex<TaskInner>,
    pub id: TaskId,
    pub state: AtomicTaskState,
    pub kind: TaskKind,
    /// Physical address of the L4 page table for this task.
    /// For kernel tasks, this is the kernel CR3.
    /// For Loading stubs, this is 0 until the loader sets it via store(Release).
    pub cr3: AtomicU64,
    /// Exit code set by sys_exit.
    pub exit_code: AtomicU64,
    /// Task waiting for this task to exit (set by sys_waitpid).
    pub exit_waiter: Mutex<Option<(Arc<Task>, u32)>>,
    /// Task waiting for this task to leave Loading state (set by sys_wait_task_ready).
    pub ready_waiter: Mutex<Option<(Arc<Task>, u32)>>,
    /// Send endpoint ID to notify on exit. 0 = unset.
    pub exit_notification_ep: AtomicU64,
    /// Send endpoint ID to notify on hardware fault (page fault, GPF, #DE). 0 = unset.
    pub fault_ep: AtomicU64,
    /// Number of scheduler quanta this task has consumed. One tick ≈ 1 ms.
    pub cpu_ticks: AtomicU64,
    /// TSC value when this task was last scheduled in. 0 = never run.
    pub slice_start_tsc: AtomicU64,
    /// Accumulated CPU time in nanoseconds.
    pub cpu_ns: AtomicU64,
    /// Human-readable name (up to 32 bytes, not null-terminated).
    pub name: [u8; 32],
    pub name_len: u8,
    /// Scheduling priority (Priority::from_u8(task.priority.load(Relaxed))).
    pub priority: AtomicU8,
    /// Parent task ID; None for kernel tasks and init_task.
    pub parent_id: Option<TaskId>,
}

impl Task {
    /// Create a new kernel-mode task.
    ///
    /// `arg` is passed to the entry function via `rdi` (the first SysV AMD64 argument
    /// register). The trampoline preserves `rdi` and calls the entry via `r15`.
    pub fn new(entry: fn() -> !, arg: u64, priority: Priority, parent_id: Option<TaskId>) -> Self {
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
            r15: entry as usize as u64, // trampoline calls entry via r15
            r14: 0, r13: 0, r12: 0, r11: 0, r10: 0, r9: 0, r8: 0,
            rdi: arg, rsi: 0, rbp: 0, rbx: 0, rdx: 0, rcx: 0, rax: 0,
            rip: task_trampoline as *const() as u64,
            cs,
            rflags: 0x200, // IF=1 (interrupts enabled on entry)
            rsp: stack_top, // after iretq, task uses full stack from the top
            ss,
        };

        let mut name = [0u8; 32];
        name[..6].copy_from_slice(b"kernel");

        Task {
            inner: Mutex::new(TaskInner {
                context,
                kernel_stack: Some(stack),
                kernel_stack_top: stack_top,
                user_page_table: None,
                user_vmas: NoditMap::new(),
                owned_endpoints: Vec::new(),
                registered_services: Vec::new(),
            }),
            id: TaskId::new(),
            state: AtomicTaskState::new(TaskState::Initializing),
            kind: TaskKind::Kernel,
            cr3: AtomicU64::new(cr3),
            exit_code: AtomicU64::new(0),
            exit_waiter: Mutex::new(None),
            ready_waiter: Mutex::new(None),
            exit_notification_ep: AtomicU64::new(0),
            fault_ep: AtomicU64::new(0),
            cpu_ticks: AtomicU64::new(0),
            slice_start_tsc: AtomicU64::new(0),
            cpu_ns: AtomicU64::new(0),
            name,
            name_len: 6,
            priority: AtomicU8::new(priority as u8),
            parent_id,
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
    #[allow(clippy::too_many_arguments)]
    pub fn new_user(
        entry_rip: u64,
        user_rsp: u64,
        page_table: PhysFrame,
        cr3: u64,
        user_cs: u16,
        user_ss: u16,
        user_vmas: NoditMap<u64, Interval<u64>, VmaEntry>,
        arg: u64,
        task_name: &[u8],
        priority: Priority,
        parent_id: Option<TaskId>,
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

        let name_len = task_name.len().min(32) as u8;
        let mut name = [0u8; 32];
        name[..name_len as usize].copy_from_slice(&task_name[..name_len as usize]);

        Task {
            inner: Mutex::new(TaskInner {
                context,
                kernel_stack: Some(kernel_stack),
                kernel_stack_top,
                user_page_table: Some(page_table),
                user_vmas,
                owned_endpoints: Vec::new(),
                registered_services: Vec::new(),
            }),
            id: TaskId::new(),
            state: AtomicTaskState::new(TaskState::Initializing),
            kind: TaskKind::User,
            cr3: AtomicU64::new(cr3),
            exit_code: AtomicU64::new(0),
            exit_waiter: Mutex::new(None),
            ready_waiter: Mutex::new(None),
            exit_notification_ep: AtomicU64::new(0),
            fault_ep: AtomicU64::new(0),
            cpu_ticks: AtomicU64::new(0),
            slice_start_tsc: AtomicU64::new(0),
            cpu_ns: AtomicU64::new(0),
            name,
            name_len,
            priority: AtomicU8::new(priority as u8),
            parent_id,
        }
    }

    /// Create a new user-mode thread that shares the parent's address space.
    ///
    /// Unlike `new_user`, this does NOT take ownership of any page table frame —
    /// the parent process owns and will free the page table.
    #[allow(clippy::too_many_arguments)]
    pub fn new_thread(
        entry_rip: u64,
        user_rsp: u64,
        cr3: u64,
        user_cs: u16,
        user_ss: u16,
        user_vmas: NoditMap<u64, Interval<u64>, VmaEntry>,
        arg: u64,
        task_name: &[u8],
        priority: Priority,
        parent_id: Option<TaskId>,
    ) -> Self {
        let kernel_stack = GuardedStack::new_kernel(
            NORMAL_STACK_SIZE,
            StackId {
                _type: StackType::Normal,
                cpu_id: get_local().kernel_id,
            },
        );
        let kernel_stack_top = kernel_stack.top().as_u64();

        let context = CpuContext {
            r15: 0, r14: 0, r13: 0, r12: 0, r11: 0, r10: 0, r9: 0, r8: 0,
            rdi: arg, rsi: 0, rbp: 0, rbx: 0, rdx: 0, rcx: 0, rax: 0,
            rip: entry_rip,
            cs: user_cs as u64,
            rflags: 0x200,
            rsp: user_rsp,
            ss: user_ss as u64,
        };

        let name_len = task_name.len().min(32) as u8;
        let mut name = [0u8; 32];
        name[..name_len as usize].copy_from_slice(&task_name[..name_len as usize]);

        Task {
            inner: Mutex::new(TaskInner {
                context,
                kernel_stack: Some(kernel_stack),
                kernel_stack_top,
                user_page_table: None, // thread does NOT own/free the page table
                user_vmas,
                owned_endpoints: Vec::new(),
                registered_services: Vec::new(),
            }),
            id: TaskId::new(),
            state: AtomicTaskState::new(TaskState::Initializing),
            kind: TaskKind::User,
            cr3: AtomicU64::new(cr3),
            exit_code: AtomicU64::new(0),
            exit_waiter: Mutex::new(None),
            ready_waiter: Mutex::new(None),
            exit_notification_ep: AtomicU64::new(0),
            fault_ep: AtomicU64::new(0),
            cpu_ticks: AtomicU64::new(0),
            slice_start_tsc: AtomicU64::new(0),
            cpu_ns: AtomicU64::new(0),
            name,
            name_len,
            priority: AtomicU8::new(priority as u8),
            parent_id,
        }
    }

    /// Create a Loading stub — a placeholder in TASK_TABLE while a kernel loader
    /// task performs the ELF parse and address-space setup asynchronously.
    ///
    /// The stub is immediately visible to `sys_waitpid` and `sys_set_exit_channel`.
    /// It transitions to `Ready` (via `spawn_task_activate`) once the loader succeeds,
    /// or to `Zombie` if the load fails.
    pub fn new_loading(
        id: TaskId,
        name: [u8; 32],
        name_len: u8,
        priority: Priority,
        parent_id: Option<TaskId>,
    ) -> Arc<Task> {
        let stack = GuardedStack::new_kernel(
            NORMAL_STACK_SIZE,
            StackId {
                _type: StackType::Normal,
                cpu_id: get_local().kernel_id,
            },
        );
        let stack_top = stack.top().as_u64();

        Arc::new(Task {
            inner: Mutex::new(TaskInner {
                context: CpuContext::default(),
                kernel_stack: Some(stack),
                kernel_stack_top: stack_top,
                user_page_table: None,
                user_vmas: NoditMap::new(),
                owned_endpoints: Vec::new(),
                registered_services: Vec::new(),
            }),
            id,
            state: AtomicTaskState::new(TaskState::Loading),
            kind: TaskKind::User,
            cr3: AtomicU64::new(0),
            exit_code: AtomicU64::new(0),
            exit_waiter: Mutex::new(None),
            ready_waiter: Mutex::new(None),
            exit_notification_ep: AtomicU64::new(0),
            fault_ep: AtomicU64::new(0),
            cpu_ticks: AtomicU64::new(0),
            slice_start_tsc: AtomicU64::new(0),
            cpu_ns: AtomicU64::new(0),
            name,
            name_len,
            priority: AtomicU8::new(priority as u8),
            parent_id,
        })
    }

    /// Immediately frees the user-space page tables for this task.
    ///
    /// Safe to call from syscall context (not holding any spinlocks). Idempotent:
    /// `user_page_table.take()` returns `None` on a second call, so `TaskInner::drop`
    /// will skip the free even if it runs later via the Arc drop chain.
    pub fn free_address_space_now(&self) {
        let l4_frame = self.inner.lock().user_page_table.take();
        if let Some(frame) = l4_frame {
            let memory = crate::memory::MEMORY.get().unwrap();
            let mut phys_mem = memory.physical_memory.lock();
            unsafe { free_user_address_space(frame.start_address(), &mut phys_mem); }
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

/// Trampoline for kernel tasks: calls the entry function in r15 with rdi = arg.
/// rdi is set in CpuContext by Task::new() and preserved through the iretq,
/// so the entry function receives the arg as its first SysV AMD64 argument.
#[unsafe(no_mangle)]
#[unsafe(naked)]
extern "C" fn task_trampoline() -> ! {
    core::arch::naked_asm!(
        "call r15",  // Call entry (fn ptr in r15), rdi = arg (preserved)
        "ud2",       // Should not return
    )
}
