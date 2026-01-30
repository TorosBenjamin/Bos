use crate::gdt::Gdt;
use crate::limine_requests::MP_REQUEST;
use crate::task::local_scheduler::RunQueue;
use alloc::boxed::Box;
use core::cell::UnsafeCell;
use core::default::Default;
use core::ptr::NonNull;
use core::sync::atomic::AtomicU64;
use force_send_sync::SendSync;
use limine::mp::Cpu;
use limine::response::MpResponse;
use spin::{Lazy, Mutex, Once};
use x2apic::lapic::LocalApic;
use x86_64::VirtAddr;
use x86_64::registers::model_specific::GsBase;
use x86_64::structures::idt::InterruptDescriptorTable;
use x86_64::structures::tss::TaskStateSegment;

pub struct CpuLocalData {
    pub kernel_id: u32,
    pub local_apic_id: u32,

    pub tss: Once<UnsafeCell<TaskStateSegment>>,
    pub gdt: Once<Gdt>,
    pub idt: Once<InterruptDescriptorTable>,

    pub local_apic: Once<UnsafeCell<SendSync<LocalApic>>>,
    pub run_queue: Once<Mutex<RunQueue>>,

    pub syscall_handler_stack_pointer: AtomicU64,
    pub syscall_handler_scratch: AtomicU64,
}

impl CpuLocalData {
    /// Update TSS.RSP0 so that interrupts from ring 3 use the correct kernel stack.
    ///
    /// # Safety
    /// Must only be called with interrupts disabled (e.g., from within the scheduler).
    pub unsafe fn set_tss_rsp0(&self, rsp0: u64) {
        let tss = unsafe { &mut *self.tss.get().unwrap().get() };
        tss.privilege_stack_table[0] = VirtAddr::new(rsp0);
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
    unsafe {
        GsBase::write(VirtAddr::from_ptr(ptr));
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
            syscall_handler_stack_pointer: Default::default(),
            run_queue: Once::new(),
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
