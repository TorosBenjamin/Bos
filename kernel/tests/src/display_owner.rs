use crate::TestResult;
use alloc::format;
use core::sync::atomic::Ordering;
use kernel::graphics::display::{DISPLAY_OWNER, is_display_owner};
use kernel::syscall_handlers::{sys_get_bounding_box, sys_transfer_display};
use kernel_api_types::graphics::GraphicsResult;

/// Helper: save and restore DISPLAY_OWNER around a test closure.
fn with_display_owner<F: FnOnce() -> TestResult>(owner: u64, f: F) -> TestResult {
    let saved = DISPLAY_OWNER.load(Ordering::Relaxed);
    DISPLAY_OWNER.store(owner, Ordering::Relaxed);
    let result = f();
    DISPLAY_OWNER.store(saved, Ordering::Relaxed);
    result
}

/// Tests run as kernel code without a scheduled current_task, so
/// is_display_owner() should always return false from test context.
pub fn test_no_current_task_is_not_owner() -> TestResult {
    with_display_owner(0, || {
        if is_display_owner() {
            TestResult::Failed("is_display_owner() returned true with no current_task".into())
        } else {
            TestResult::Ok
        }
    })
}

/// With DISPLAY_OWNER set to u64::MAX (no owner), is_display_owner() returns false.
pub fn test_no_owner_is_not_owner() -> TestResult {
    with_display_owner(u64::MAX, || {
        if is_display_owner() {
            TestResult::Failed("is_display_owner() returned true with no owner set".into())
        } else {
            TestResult::Ok
        }
    })
}

/// A non-owner calling sys_get_bounding_box gets PermissionDenied.
pub fn test_non_owner_get_bounding_box_rejected() -> TestResult {
    with_display_owner(u64::MAX, || {
        let ret = sys_get_bounding_box(0, 0, 0, 0, 0, 0);
        if ret == GraphicsResult::PermissionDenied as u64 {
            TestResult::Ok
        } else {
            TestResult::Failed(format!(
                "Expected PermissionDenied ({}), got {}",
                GraphicsResult::PermissionDenied as u64,
                ret
            ))
        }
    })
}

/// A non-owner calling sys_transfer_display returns 1 (not owner).
pub fn test_transfer_display_not_owner() -> TestResult {
    with_display_owner(0xBEEF, || {
        let ret = sys_transfer_display(0xDEAD, 0, 0, 0, 0, 0);
        if ret == 1 {
            TestResult::Ok
        } else {
            TestResult::Failed(format!("Expected 1 (not owner), got {}", ret))
        }
    })
}

/// Calling sys_transfer_display with a non-existent target task returns 2,
/// but only if the caller is the owner. Since tests have no current_task,
/// we can't be the owner — so this will return 1. We verify that behavior.
pub fn test_transfer_display_no_current_task() -> TestResult {
    // Even if DISPLAY_OWNER matches nothing, current_task is None so
    // is_display_owner() returns false -> returns 1.
    with_display_owner(u64::MAX, || {
        let ret = sys_transfer_display(0xDEADBEEF, 0, 0, 0, 0, 0);
        if ret == 1 {
            TestResult::Ok
        } else {
            TestResult::Failed(format!("Expected 1 (not owner), got {}", ret))
        }
    })
}

/// Verify that DISPLAY_OWNER can be atomically stored and loaded.
pub fn test_display_owner_atomic() -> TestResult {
    let saved = DISPLAY_OWNER.load(Ordering::Relaxed);

    DISPLAY_OWNER.store(42, Ordering::Relaxed);
    let val = DISPLAY_OWNER.load(Ordering::Relaxed);
    DISPLAY_OWNER.store(saved, Ordering::Relaxed);

    if val == 42 {
        TestResult::Ok
    } else {
        TestResult::Failed(format!("Expected 42, got {}", val))
    }
}
