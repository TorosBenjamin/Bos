use crate::memory::cpu_local_data::get_local;
use crate::memory::guarded_stack::{
    EXCEPTION_HANDLER_STACK_SIZE, GuardedStack, StackId, StackType,
};
use core::cell::UnsafeCell;
use num_enum::IntoPrimitive;
use x86_64::instructions::segmentation::{CS, SS, Segment};
use x86_64::instructions::tables::load_tss;
use x86_64::registers::segmentation::SegmentSelector;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable};
use x86_64::structures::tss::TaskStateSegment;

#[derive(Debug, IntoPrimitive)]
#[repr(u8)]
pub enum IstStackIndexes {
    Exception,
}

pub struct Gdt {
    gdt: GlobalDescriptorTable,
    kernel_code_selector: SegmentSelector,
    kernel_data_selector: SegmentSelector,
    user_data_selector: SegmentSelector,
    user_code_selector: SegmentSelector,
    tss_selector: SegmentSelector,
}

impl Gdt {
    pub fn user_code_selector(&self) -> SegmentSelector {
        self.user_code_selector
    }

    pub fn user_data_selector(&self) -> SegmentSelector {
        self.user_data_selector
    }
}

pub fn init() {
    let local = get_local();
    let tss_cell = local.tss.call_once(|| {
        let mut tss = TaskStateSegment::new();
        tss.interrupt_stack_table[u8::from(IstStackIndexes::Exception) as usize] =
            GuardedStack::new_kernel(
                EXCEPTION_HANDLER_STACK_SIZE,
                StackId {
                    _type: StackType::ExceptionHandler,
                    cpu_id: local.kernel_id,
                },
            )
            .top();
        UnsafeCell::new(tss)
    });

    // Safety: TSS is only mutated via set_tss_rsp0 with interrupts disabled;
    // here we only need a shared reference for tss_segment().
    let tss = unsafe { &*tss_cell.get() };

    let gdt = local.gdt.call_once(|| {
        let mut gdt = GlobalDescriptorTable::new();
        // GDT layout (order matters for SYSCALL/SYSRET):
        // 0x00: Null
        // 0x08: Kernel Code (index 1)
        // 0x10: Kernel Data (index 2)
        // 0x18: User Data   (index 3) — must be before User Code for SYSRET
        // 0x20: User Code   (index 4)
        // 0x28: TSS         (index 5-6, 16 bytes)
        let kernel_code_selector = gdt.append(Descriptor::kernel_code_segment());
        let kernel_data_selector = gdt.append(Descriptor::kernel_data_segment());
        let user_data_selector = gdt.append(Descriptor::user_data_segment());
        let user_code_selector = gdt.append(Descriptor::user_code_segment());
        let tss_selector = gdt.append(Descriptor::tss_segment(tss));
        Gdt {
            gdt,
            kernel_code_selector,
            kernel_data_selector,
            user_data_selector,
            user_code_selector,
            tss_selector,
        }
    });

    gdt.gdt.load();

    // Reload selectors
    unsafe { CS::set_reg(gdt.kernel_code_selector) };
    unsafe { SS::set_reg(gdt.kernel_data_selector) };
    unsafe { load_tss(gdt.tss_selector) };
}
