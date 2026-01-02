use alloc::format;
use alloc::string::String;
use ez_paging::Owned4KibFrame;
use kernel::memory::MEMORY;
use kernel::memory::physical_memory::{KernelMemoryUsageType, MemoryType, PhysicalMemory};
use crate::TestResult;

/// Clean up allocated kernel frames
fn with_kernel_frame<F>(mut f: F) -> TestResult
where
    F: FnMut(&mut PhysicalMemory, &Owned4KibFrame) -> TestResult,
{
    let mut pm = MEMORY.get().unwrap().physical_memory.lock();
    let mut allocator = pm.get_kernel_frame_allocator();

    let frame = match allocator.allocate_frame_4kib() {
        Some(f) => f,
        None => return TestResult::Failed(String::from("Failed to allocate frame")),
    };

    let result = f(&mut pm, &frame);

    let _ = pm.free_frame(
        frame,
        MemoryType::UsedByKernel(KernelMemoryUsageType::PageTables),
    );

    result
}

pub fn alloc_one_frame() -> TestResult {
    with_kernel_frame(|pm, frame| {
        if pm.is_frame_allocated(frame) {
            TestResult::Ok
        } else {
            TestResult::Failed(String::from("Failed to allocate frame"))
        }

    })
}

pub fn frame_alignment() -> TestResult {
    with_kernel_frame(|_, frame| {
        if frame.start_address().as_u64() % 0x1000 == 0 {
            TestResult::Ok
        } else {
            TestResult::Failed(String::from("Physical frame not aligned to 4KiB"))
        }
    })
}

pub fn kernel_type() -> TestResult {
    with_kernel_frame(|pm, frame| {
        let addr = frame.start_address().as_u64();

        let entry = pm.map_mut().iter().find(|(interval, t)| {
            *interval.start() <= addr &&
                addr < *interval.end() &&
                **t == MemoryType::UsedByKernel(KernelMemoryUsageType::PageTables)
        });

        if entry.is_some() {
            TestResult::Ok
        } else {
            TestResult::Failed(String::from("MemoryType not updated"))
        }
    })
}

pub fn user_type() -> TestResult {
    let mut pm = MEMORY.get().unwrap().physical_memory.lock();
    let mut allocator = pm.get_user_mode_frame_allocator();

    let frame = match allocator.allocate_frame_4kib() {
        Some(f) => f,
        None => return TestResult::Failed(String::from("Failed to allocate user frame")),
    };

    let addr = frame.start_address().as_u64();

    let found = {
        pm.map_mut().iter().any(|(interval, t)| {
            *interval.start() <= addr
                && addr < *interval.end()
                && *t == MemoryType::UsedByUserMode
        })
    };

    // Now this is legal
    let _ = pm.free_frame(frame, MemoryType::UsedByUserMode);

    if found {
        TestResult::Ok
    } else {
        TestResult::Failed(String::from("MemoryType not updated"))
    }
}


pub fn exhaustion() -> TestResult {
    let mut pm = MEMORY.get().unwrap().physical_memory.lock();
    let mut allocator = pm.get_kernel_frame_allocator();

    let mut frames = heapless::Vec::<Owned4KibFrame, 1024>::new();

    while let Some(frame) = allocator.allocate_frame_4kib() {
        if frames.push(frame).is_err() {
            break;
        }
    }

    let count = frames.len();

    // Clean up used frames
    for frame in frames {
        let _ = pm.free_frame(
            frame,
            MemoryType::UsedByKernel(KernelMemoryUsageType::PageTables),
        );
    }

    if count > 0 {
        TestResult::Ok
    } else {
        TestResult::Failed(String::from("No frames allocated"))
    }
}

pub fn duplicate_allocation() -> TestResult {
    let mut pm = MEMORY.get().unwrap().physical_memory.lock();
    let mut seen_addrs = heapless::Vec::<u64, 1024>::new();

    loop {
        // Borrow allocator only inside this scope
        let frame = {
            let mut allocator = pm.get_kernel_frame_allocator();
            allocator.allocate_frame_4kib()
        };

        let frame = match frame {
            Some(f) => f,
            None => break, // no more frames
        };

        let addr = frame.start_address().as_u64();

        if seen_addrs.iter().any(|&x| x == addr) {
            let _ = pm.free_frame(frame, MemoryType::UsedByKernel(KernelMemoryUsageType::PageTables));
            return TestResult::Failed(String::from("Duplicate frame allocated"));
        }

        if seen_addrs.push(addr).is_err() {
            let _ = pm.free_frame(frame, MemoryType::UsedByKernel(KernelMemoryUsageType::PageTables));
            break;
        }

        // free immediately
        let _ = pm.free_frame(frame, MemoryType::UsedByKernel(KernelMemoryUsageType::PageTables));
    }

    TestResult::Ok
}

pub fn free_and_reuse_kernel_frame() -> TestResult {
    let mut pm = MEMORY.get().unwrap().physical_memory.lock();

    // Allocate frame
    let frame = {
        let mut allocator = pm.get_kernel_frame_allocator();
        allocator
            .allocate_frame_4kib()
            .expect("Failed to allocate frame")
    };

    let addr = frame.start_address().as_u64();

    // Free the frame
    match pm.free_frame(frame, MemoryType::UsedByKernel(KernelMemoryUsageType::PageTables)) {
        Ok(_) => {} // success, do nothing
        Err(e) => return TestResult::Failed(format!("Free failed: {:?}", e)),
    }


    // Allocate again
    let new_frame = {
        let mut allocator = pm.get_kernel_frame_allocator();
        allocator
            .allocate_frame_4kib()
            .expect("Failed to reallocate frame")
    };

    if new_frame.start_address().as_u64() == addr {
        TestResult::Ok
    } else {
        TestResult::Failed(String::from("Freed frame was not reused"))
    }
}