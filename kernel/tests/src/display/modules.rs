use alloc::format;
use crate::TestResult;
use kernel::limine_requests::MODULE_REQUEST;

pub fn test_init_task_module_exists() -> TestResult {
    let response = MODULE_REQUEST.get_response().unwrap();
    let found = response.modules().iter().any(|m| m.path().to_bytes() == b"/init_task");
    if found {
        TestResult::Ok
    } else {
        TestResult::Failed(format!("init_task module not found in MODULE_REQUEST"))
    }
}

pub fn test_display_server_module_exists() -> TestResult {
    let response = MODULE_REQUEST.get_response().unwrap();
    let found = response.modules().iter().any(|m| m.path().to_bytes() == b"/display_server");
    if found {
        TestResult::Ok
    } else {
        TestResult::Failed(format!("display_server module not found in MODULE_REQUEST"))
    }
}

pub fn test_nonexistent_module_missing() -> TestResult {
    let response = MODULE_REQUEST.get_response().unwrap();
    let found = response.modules().iter().any(|m| m.path().to_bytes() == b"/does_not_exist");
    if !found {
        TestResult::Ok
    } else {
        TestResult::Failed(format!("nonexistent module unexpectedly found"))
    }
}

pub fn test_module_has_nonzero_size() -> TestResult {
    let response = MODULE_REQUEST.get_response().unwrap();
    let module = response.modules().iter().find(|m| m.path().to_bytes() == b"/display_server");
    match module {
        Some(m) if m.size() > 0 => TestResult::Ok,
        Some(m) => TestResult::Failed(format!("display_server module has size {}", m.size())),
        None => TestResult::Failed(format!("display_server module not found")),
    }
}
