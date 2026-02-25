pub mod timer;

use crate::TestResult;
use x86_64::registers::segmentation::{Segment, CS};
use x86_64::instructions::tables::sidt;

pub fn gdt_loaded() -> TestResult {
    let cs = CS::get_reg();
    // In our GDT, the first segment is null, the second is kernel code.
    // Segment selectors have the index in bits 3-15.
    // Index 1 (kernel code) would be 0x08 (0x01 << 3).
    if cs.0 != 0 {
        TestResult::Ok
    } else {
        TestResult::Failed(alloc::format!("CS register is 0, GDT might not be loaded correctly. CS: {:?}", cs))
    }
}

pub fn idt_loaded() -> TestResult {
    let idtr = sidt();
    if idtr.limit > 0 {
        TestResult::Ok
    } else {
        TestResult::Failed(alloc::format!("IDT limit is 0, IDT might not be loaded correctly. IDTR: {:?}", idtr))
    }
}

pub fn breakpoint_exception() -> TestResult {
    // This will trigger the breakpoint handler.
    // If it works, it just continues.
    // If it fails, it might panic or hang.
    x86_64::instructions::interrupts::int3();
    TestResult::Ok
}
