use crate::gdt::Gdt;
use crate::limine_requests::MP_REQUEST;
use crate::task::local_scheduler::RunQueue;
use crate::task::task::CpuContext;
use alloc::boxed::Box;
use core::cell::UnsafeCell;
use core::default::Default;
use core::mem::offset_of;
use core::ptr::NonNull;
use atomic_enum::atomic_enum;
use core::sync::atomic::{AtomicPtr, AtomicU64, AtomicU8};
use force_send_sync::SendSync;
use limine::mp::Cpu;
use limine::response::MpResponse;
use spin::{Lazy, Mutex, Once};
use x2apic::lapic::LocalApic;
use x86_64::VirtAddr;
use x86_64::registers::model_specific::{GsBase, KernelGsBase};
use x86_64::structures::idt::InterruptDescriptorTable;
use x86_64::structures::tss::TaskStateSegment;

#[atomic_enum]
#[derive(PartialEq)]
pub enum CpuState {
    /// Hardware init in progress (GDT/IDT/APIC/run-queue not all done yet).
    Initializing,
    /// Fully initialized — timer armed, interrupts about to be / are enabled.
    /// Tasks may be dispatched to this CPU.
    Ready,
    /// This CPU has panicked and should be ignored by the scheduler.
    Crashed,
}

pub struct CpuLocalData {
    pub kernel_id: u32,
    pub local_apic_id: u32,

    pub tss: Once<UnsafeCell<TaskStateSegment>>,
    pub gdt: Once<Gdt>,
    pub idt: Once<InterruptDescriptorTable>,

    pub local_apic: Once<UnsafeCell<SendSync<LocalApic>>>,
    pub run_queue: Once<Mutex<RunQueue>>,

    pub syscall_handler_scratch: AtomicU64,
    /// Top of the current task's kernel stack. Updated on every context switch (mirrors TSS.RSP0).
    /// Used by the syscall handler to switch from user to kernel stack without a per-CPU stack.
    pub current_task_kernel_stack_top: AtomicU64,
    /// Pointer to the current task's CpuContext (used by timer handler for save/restore)
    pub current_context_ptr: AtomicPtr<CpuContext>,
    /// Set to 1 while inside a syscall handler. When the timer fires with this flag set,
    /// it skips saving registers (user state was already saved at syscall entry).
    pub in_syscall_handler: AtomicU8,
    /// Number of tasks currently in the ready queue (not counting the running task).
    /// Updated without holding the run queue lock so other CPUs can read it cheaply.
    pub ready_count: core::sync::atomic::AtomicUsize,
    /// Lifecycle state — guards task dispatch and crash handling.
    pub state: AtomicCpuState,
}

/// Offset of current_context_ptr in CpuLocalData for assembly access
pub const CURRENT_CONTEXT_PTR_OFFSET: usize = offset_of!(CpuLocalData, current_context_ptr);
/// Offset of in_syscall_handler in CpuLocalData for assembly access
pub const IN_SYSCALL_HANDLER_OFFSET: usize = offset_of!(CpuLocalData, in_syscall_handler);
/// Offset of current_task_kernel_stack_top in CpuLocalData for assembly access
pub const CURRENT_TASK_KERNEL_STACK_TOP_OFFSET: usize = offset_of!(CpuLocalData, current_task_kernel_stack_top);

impl CpuLocalData {
    /// Update TSS.RSP0 and the per-CPU kernel stack top for the current task.
    ///
    /// TSS.RSP0 is used by the CPU on interrupt entry from ring 3.
    /// `current_task_kernel_stack_top` is read by the syscall handler to switch stacks.
    /// Both must stay in sync so interrupts and syscalls land on the same task stack.
    ///
    /// # Safety
    /// Must only be called with interrupts disabled (e.g., from within the scheduler).
    pub unsafe fn set_tss_rsp0(&self, rsp0: u64) {
        let tss = unsafe { &mut *self.tss.get().unwrap().get() };
        tss.privilege_stack_table[0] = VirtAddr::new(rsp0);
        self.current_task_kernel_stack_top.store(rsp0, core::sync::atomic::Ordering::Relaxed);
    }
}

// Safety:
// - Per-CPU data
// - Accessed only via GS base
// - No cross-CPU access
unsafe impl Sync for CpuLocalData {}

fn mp_response() -> &'static MpResponse {
    MP_REQUEST.get_response().expect("expected MP response")
}

static CPU_LOCAL_DATA: Lazy<Box<[Once<CpuLocalData>]>> =
    Lazy::new(|| mp_response().cpus().iter().map(|_| Once::new()).collect());

fn write_gs_base(ptr: &'static CpuLocalData) {
    let vaddr = VirtAddr::from_ptr(ptr);
    unsafe {
        // GS.Base = kernel ptr — used immediately by get_local() in kernel mode.
        // KernelGsBase = 0 — represents user's initial GS (zero).
        // swapgs on ring-3→ring-0 transitions restores GS.Base to vaddr;
        // swapgs on ring-0→ring-3 sets GS.Base to 0 for user mode.
        GsBase::write(vaddr);
        KernelGsBase::write(VirtAddr::new(0));
    }
}

/// Initializes the item in 'CPU_LOCAL_DATA' and GS.Base
fn init_cpu(kernel_id: u32, local_apic_id: u32) {
    write_gs_base(
        CPU_LOCAL_DATA[kernel_id as usize].call_once(|| CpuLocalData {
            kernel_id,
            local_apic_id,
            tss: Once::new(),
            gdt: Once::new(),
            idt: Once::new(),
            local_apic: Once::new(),
            syscall_handler_scratch: Default::default(),
            current_task_kernel_stack_top: AtomicU64::new(0),
            run_queue: Once::new(),
            current_context_ptr: AtomicPtr::new(core::ptr::null_mut()),
            in_syscall_handler: AtomicU8::new(0),
            ready_count: core::sync::atomic::AtomicUsize::new(0),
            state: AtomicCpuState::new(CpuState::Initializing),
        }),
    )
}

pub fn cpus_count() -> usize {
    mp_response().cpus().len()
}

pub fn local_apic_id_of(kernel_assigned_id: u32) -> u32 {
    CPU_LOCAL_DATA[kernel_assigned_id as usize]
        .get()
        .unwrap()
        .local_apic_id
}

pub fn try_get_local() -> Option<&'static CpuLocalData> {
    let ptr = NonNull::new(GsBase::read().as_mut_ptr::<CpuLocalData>())?;
    // Safety: we only wrote to GsBase using `write_gs_base`, which ensures that the pointer is `&'static CpuLocalData`
    unsafe { Some(ptr.as_ref()) }
}

pub fn get_local() -> &'static CpuLocalData {
    try_get_local().unwrap()
}

pub fn get_cpu(id: u32) -> &'static CpuLocalData {
    CPU_LOCAL_DATA[id as usize].get().unwrap()
}

/// Mark the current CPU as fully initialized and ready to accept tasks.
pub fn mark_current_cpu_ready() {
    get_local().state.store(CpuState::Ready, core::sync::atomic::Ordering::Release);
}

/// Mark the current CPU as crashed so the scheduler stops dispatching to it.
pub fn mark_current_cpu_crashed() {
    if let Some(cpu) = try_get_local() {
        cpu.state.store(CpuState::Crashed, core::sync::atomic::Ordering::Release);
    }
}

/// Returns `Some` only if the CPU is fully initialized and accepting tasks.
pub fn try_get_ready_cpu(id: u32) -> Option<&'static CpuLocalData> {
    let cpu = CPU_LOCAL_DATA.get(id as usize)?.get()?;
    if cpu.state.load(core::sync::atomic::Ordering::Acquire) != CpuState::Ready {
        return None;
    }
    Some(cpu)
}

/// Initialize CPU local data for the BSP
///
/// # Safety:
/// Must be called on the AP
pub unsafe fn init_bsp() {
    // Always assign 0 to BSP
    init_cpu(0, mp_response().bsp_lapic_id())
}

pub unsafe fn init_ap(cpu: &Cpu) {
    let local_apic_id = cpu.lapic_id;
    init_cpu(
        // Get the position within the array (0 is BSP)
        mp_response()
            .cpus()
            .iter()
            .filter(|cpu| cpu.lapic_id != mp_response().bsp_lapic_id())
            .position(|cpu| cpu.lapic_id == local_apic_id)
            .expect("CPUs array should contain this AP") as u32
            + 1,
        local_apic_id,
    )
}
