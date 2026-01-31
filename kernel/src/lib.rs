#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

pub mod acpi;
pub mod apic;
pub mod gdt;
pub mod graphics;
pub mod drivers;
pub mod ioapic;
pub mod limine_requests;
pub mod memory;
pub mod raw_syscall_handler;
pub mod syscall_handlers;
pub mod task;
pub mod user_task_from_elf;
pub mod interrupt;

pub mod exceptions;

pub mod time;

pub mod logger;
pub mod consts;

pub mod reexports {
    pub use x86_64;
}

pub fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}

pub fn project_version() -> &'static str {
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../VERSION")).trim()
}