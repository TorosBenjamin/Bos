use alloc::format;
use kernel::memory::user_vaddr::{allocate_user_pages, free_user_pages};
use kernel::consts::{USER_MAX, USER_MIN};
use nodit::{Interval, NoditSet};
use crate::TestResult;

pub fn test_user_vaddr_allocate() -> TestResult {
    let mut set: NoditSet<u64, Interval<u64>> = NoditSet::default();
    let addr = allocate_user_pages(&mut set, 1);
    match addr {
        Some(a) if a >= USER_MIN && a + 4096 - 1 <= USER_MAX && a % 4096 == 0 => TestResult::Ok,
        Some(a) => TestResult::Failed(format!("address {:#x} out of user range or misaligned", a)),
        None => TestResult::Failed(format!("allocation returned None")),
    }
}

pub fn test_user_vaddr_free() -> TestResult {
    let mut set: NoditSet<u64, Interval<u64>> = NoditSet::default();
    let addr = allocate_user_pages(&mut set, 1).unwrap();
    if !free_user_pages(&mut set, addr, 4096) {
        return TestResult::Failed(format!("free_user_pages returned false"));
    }
    // After freeing, the set should be empty — allocating again should return the same or similar address
    if set.iter().count() != 0 {
        return TestResult::Failed(format!("set not empty after free"));
    }
    TestResult::Ok
}

pub fn test_user_vaddr_no_overlap() -> TestResult {
    let mut set: NoditSet<u64, Interval<u64>> = NoditSet::default();
    let addr1 = allocate_user_pages(&mut set, 1).unwrap();
    let addr2 = allocate_user_pages(&mut set, 1).unwrap();
    if addr1 == addr2 {
        return TestResult::Failed(format!("two allocations returned same address {:#x}", addr1));
    }
    // Ranges must not overlap
    let end1 = addr1 + 4096;
    let end2 = addr2 + 4096;
    if addr1 < end2 && addr2 < end1 {
        return TestResult::Failed(format!(
            "allocations overlap: {:#x}..{:#x} and {:#x}..{:#x}",
            addr1, end1, addr2, end2
        ));
    }
    TestResult::Ok
}

pub fn test_mmap_flags_in_api() -> TestResult {
    if kernel_api_types::MMAP_WRITE != 1 {
        return TestResult::Failed(format!("MMAP_WRITE != 1"));
    }
    if kernel_api_types::MMAP_EXEC != 2 {
        return TestResult::Failed(format!("MMAP_EXEC != 2"));
    }
    TestResult::Ok
}
