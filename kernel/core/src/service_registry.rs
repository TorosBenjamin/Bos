use alloc::collections::BTreeMap;
use kernel_api_types::MAX_SERVICE_NAME_LEN;
use spin::Mutex;
use crate::task::task::TaskId;

pub type ServiceName = [u8; MAX_SERVICE_NAME_LEN];

struct ServiceEntry {
    send_endpoint_id: u64,
    #[allow(dead_code)]
    owner_task_id: TaskId,
}

static SERVICE_REGISTRY: Mutex<BTreeMap<ServiceName, ServiceEntry>> =
    Mutex::new(BTreeMap::new());

/// Register a send endpoint under the given name.
/// Returns `Err(SVC_ERR_ALREADY_REGISTERED)` if the name is already taken.
pub fn register(name_bytes: &[u8], send_ep: u64, owner: TaskId) -> Result<(), u64> {
    let mut name: ServiceName = [0u8; MAX_SERVICE_NAME_LEN];
    let copy_len = name_bytes.len().min(MAX_SERVICE_NAME_LEN);
    name[..copy_len].copy_from_slice(&name_bytes[..copy_len]);

    let mut registry = SERVICE_REGISTRY.lock();
    if registry.contains_key(&name) {
        return Err(kernel_api_types::SVC_ERR_ALREADY_REGISTERED);
    }
    registry.insert(name, ServiceEntry { send_endpoint_id: send_ep, owner_task_id: owner });
    Ok(())
}

/// Look up a service by name, returning its send endpoint ID if found.
pub fn lookup(name_bytes: &[u8]) -> Option<u64> {
    let mut name: ServiceName = [0u8; MAX_SERVICE_NAME_LEN];
    let copy_len = name_bytes.len().min(MAX_SERVICE_NAME_LEN);
    name[..copy_len].copy_from_slice(&name_bytes[..copy_len]);

    SERVICE_REGISTRY.lock().get(&name).map(|e| e.send_endpoint_id)
}

/// Remove all services registered by the given task (called on task exit).
pub fn unregister_all_for_task(owner: TaskId) {
    let mut registry = SERVICE_REGISTRY.lock();
    registry.retain(|_, entry| entry.owner_task_id != owner);
}
