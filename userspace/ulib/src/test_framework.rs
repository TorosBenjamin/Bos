/// Minimal sequential test runner for `no_std` userspace integration tests.
///
/// Each test is a `fn() -> bool` that returns `true` on pass, `false` on fail.
/// After all tests run, call `finish()` to exit QEMU with the appropriate code:
///   0x10 → all passed (QEMU exit 33)
///   0x11 → any failed  (QEMU exit 35)
pub struct TestRunner {
    pub passed: u32,
    pub failed: u32,
    index: u32,
}

const PASS_TAG: u64 = 0x5041_5353; // "PASS"
const FAIL_TAG: u64 = 0x4641_494C; // "FAIL"

impl TestRunner {
    pub const fn new() -> Self {
        TestRunner { passed: 0, failed: 0, index: 0 }
    }

    /// Run a single test function and log the result via `sys_debug_log`.
    ///
    /// Serial output:  `DBG[<test_index>]: 0x50415353`  (PASS)
    ///              or `DBG[<test_index>]: 0x4641494c`  (FAIL)
    pub fn run(&mut self, f: fn() -> bool) {
        let idx = self.index;
        self.index += 1;
        if f() {
            self.passed += 1;
            crate::sys_debug_log(PASS_TAG, idx as u64);
        } else {
            self.failed += 1;
            crate::sys_debug_log(FAIL_TAG, idx as u64);
        }
    }

    /// Shut down QEMU: exit code 0x10 if all passed, 0x11 if any failed.
    pub fn finish(self) -> ! {
        if self.failed == 0 {
            crate::sys_shutdown(0x10)
        } else {
            crate::sys_shutdown(0x11)
        }
    }
}
