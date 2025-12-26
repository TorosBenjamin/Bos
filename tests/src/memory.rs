use core::ptr::from_mut;
use x86_64::structures::paging::PhysFrame;
use kernel::memory::MEMORY;
use kernel::memory::physical_memory::{KernelMemoryUsageType, MemoryType};
use crate::TestResult;
pub fn alloc_one_frame() -> TestResult {
    let mut pm  = MEMORY.get().unwrap().physical_memory.lock();
    let mut allocator = pm.get_kernel_frame_allocator();
    let frame = allocator.allocate_frame_4kib();
    if frame.is_some() {TestResult::Ok} else { TestResult::Failed("Failed to allocate frame!") }
}

pub fn frame_alignment() -> TestResult {
    let mut pm = MEMORY.get().unwrap().physical_memory.lock();
    let mut allocator = pm.get_kernel_frame_allocator();

    let frame = allocator.allocate_frame_4kib().unwrap();
    if frame.start_address().as_u64() % 0x1000 == 0 {
        TestResult::Ok
    } else {
        TestResult::Failed("Physical frame not aligned to 4KiB")
    }
}

pub fn exhaustion() -> TestResult {
    let mut pm = MEMORY.get().unwrap().physical_memory.lock();
    let mut allocator = pm.get_kernel_frame_allocator();

    let mut count = 0;
    while allocator.allocate_frame_4kib().is_some() {
        count += 1;
    }

    if count > 0 { TestResult::Ok } else { TestResult::Failed("No frames allocated") }
}

pub fn kernel_type() -> TestResult {
    let mut pm = MEMORY.get().unwrap().physical_memory.lock();
    let mut allocator = pm.get_kernel_frame_allocator();

    let frame = allocator.allocate_frame_4kib().unwrap();
    let addr = frame.start_address().as_u64();

    let entry = pm.map_mut().iter().find(|(interval, t)| {
        let start = *interval.start();
        let end = *interval.end();
        start <= addr && addr < end && **t == MemoryType::UsedByKernel(KernelMemoryUsageType::PageTables)
    });

    if entry.is_some() {
        TestResult::Ok
    } else {
        TestResult::Failed("MemoryType not updated")
    }
}

pub fn user_type() -> TestResult {
    let mut pm = MEMORY.get().unwrap().physical_memory.lock();
    let mut allocator = pm.get_user_mode_frame_allocator();

    let frame = allocator.allocate_frame_4kib().unwrap();
    let addr = frame.start_address().as_u64();

    let entry = pm.map_mut().iter().find(|(interval, t)| {
        let start = *interval.start();
        let end = *interval.end();
        start <= addr && addr < end && **t == MemoryType::UsedByUserMode
    });

    if entry.is_some() {
        TestResult::Ok
    } else {
        TestResult::Failed("MemoryType not updated")
    }
}