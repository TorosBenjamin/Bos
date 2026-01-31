use crate::TestResult;
use alloc::format;
use kernel::task::task::{TaskKind, TaskState};

/// Calling create_user_task_from_elf_bytes with garbage bytes should return InvalidElf.
pub fn test_spawn_error_invalid_elf() -> TestResult {
    let garbage = [0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01, 0x02, 0x03];
    match kernel::user_task_from_elf::create_user_task_from_elf_bytes(&garbage, 0) {
        Err(_) => TestResult::Ok,
        Ok(_) => TestResult::Failed("Expected InvalidElf error for garbage bytes".into()),
    }
}

/// Create a task from the Limine module ELF bytes via create_user_task_from_elf_bytes
/// and verify it has the expected properties.
pub fn test_spawn_creates_task() -> TestResult {
    // Get the ELF bytes from the Limine module
    let elf_bytes = get_user_elf_bytes();

    match kernel::user_task_from_elf::create_user_task_from_elf_bytes(elf_bytes, 0) {
        Err(e) => TestResult::Failed(format!("Failed to create task: {:?}", e)),
        Ok(task) => {
            if task.kind != TaskKind::User {
                return TestResult::Failed(format!("Expected User kind, got {:?}", task.kind));
            }

            let (current_cr3_frame, _) = x86_64::registers::control::Cr3::read();
            let current_cr3 = current_cr3_frame.start_address().as_u64();
            if task.cr3 == current_cr3 {
                return TestResult::Failed("Task CR3 matches kernel CR3".into());
            }

            let inner = task.inner.lock();
            if inner.user_vaddr_set.is_empty() {
                return TestResult::Failed("user_vaddr_set is empty".into());
            }

            if task.run_state() != TaskState::Initializing {
                return TestResult::Failed(format!(
                    "Expected Initializing state, got {:?}",
                    task.run_state()
                ));
            }

            TestResult::Ok
        }
    }
}

/// Verify that child_arg is placed in the InitialTaskFrame's rdi field.
pub fn test_spawn_child_arg() -> TestResult {
    let elf_bytes = get_user_elf_bytes();
    let arg_value: u64 = 0xDEAD_BEEF_CAFE_BABE;

    match kernel::user_task_from_elf::create_user_task_from_elf_bytes(elf_bytes, arg_value) {
        Err(e) => TestResult::Failed(format!("Failed to create task: {:?}", e)),
        Ok(task) => {
            let inner = task.inner.lock();
            // InitialTaskFrame layout: [r15..rax (15 GPRs), rip, cs, rflags, rsp, ss]
            // rdi is at index 8 (r15=0, r14=1, r13=2, r12=3, r11=4, r10=5, r9=6, r8=7, rdi=8)
            let frame_ptr = inner.rsp as *const u64;
            let rdi = unsafe { *frame_ptr.add(8) };

            if rdi != arg_value {
                return TestResult::Failed(format!(
                    "Expected rdi = {:#x}, got {:#x}",
                    arg_value, rdi
                ));
            }

            TestResult::Ok
        }
    }
}

fn get_user_elf_bytes() -> &'static [u8] {
    use core::ptr::NonNull;
    use core::ptr::slice_from_raw_parts_mut;

    let module = kernel::limine_requests::MODULE_REQUEST
        .get_response()
        .unwrap()
        .modules()
        .iter()
        .find(|m| m.path() == kernel::limine_requests::USER_LAND_PATH)
        .expect("user_land module not found");

    let ptr = NonNull::new(slice_from_raw_parts_mut(
        module.addr(),
        module.size() as usize,
    ))
    .unwrap();

    unsafe { ptr.as_ref() }
}
