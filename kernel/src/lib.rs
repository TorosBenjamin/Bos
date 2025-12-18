#![no_std]
#![cfg_attr(test, no_main)]
#![feature(abi_x86_interrupt)]
extern crate alloc;

use num_enum::IntoPrimitive;

pub const HIGHER_HALF_START: u64 = 0xFFFF800000000000;
pub const LOWER_HALF_END: u64 = 0x800000000000;

pub mod graphics;
pub mod memory;
pub mod limine_requests;
pub mod gdt;
pub mod interrupt;
pub mod task;
pub mod acpi;
pub mod apic;
pub mod nmi_handler_state;
pub mod user_land;
pub mod raw_syscall_handler;
pub mod syscall_handlers;

#[derive(Debug, IntoPrimitive)]
#[repr(u8)]
pub enum InterruptVector {
    LocalApicSpurious = 0x20,
    LocalApicTimer,
    LocalApicError,
}

pub fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}