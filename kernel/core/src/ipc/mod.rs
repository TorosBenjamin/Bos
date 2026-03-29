use alloc::collections::BTreeMap;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use spin::Mutex;
use crate::task::task::{Task, TaskState};
use crate::task::local_scheduler::EventWaiterSlot;

type WaiterQueue = Mutex<VecDeque<(Arc<Task>, u32)>>;

pub const MAX_MESSAGE_SIZE: usize = 4096;
pub const DEFAULT_CHANNEL_CAPACITY: usize = 16;
pub const MAX_CHANNEL_CAPACITY: usize = 256;

static NEXT_ENDPOINT_ID: AtomicU64 = AtomicU64::new(1);
pub static ENDPOINT_REGISTRY: Mutex<BTreeMap<u64, Endpoint>> = Mutex::new(BTreeMap::new());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointRole {
    Send,
    Recv,
}

pub struct Endpoint {
    pub role: EndpointRole,
    pub channel: Arc<Channel>,
    /// Number of handles wrapping this endpoint. When a handle is closed,
    /// the endpoint is only destroyed when this reaches 0.
    /// `sys_channel_close` (raw syscall) force-closes regardless.
    pub handle_refs: u64,
}

pub struct Channel {
    pub inner: Mutex<ChannelInner>,
    pub send_closed: AtomicBool,
    pub recv_closed: AtomicBool,
    /// Number of live send endpoints on this channel. When it drops to 0,
    /// `send_closed` is set. This allows multiple cloned send endpoints to
    /// coexist — closing one doesn't break the others.
    pub send_count: AtomicU32,
    /// Number of live recv endpoints on this channel (same semantics as send_count).
    pub recv_count: AtomicU32,
    /// Tasks sleeping waiting to receive; woken (one at a time) when try_send succeeds.
    pub recv_waiters: WaiterQueue,
    /// Tasks sleeping waiting to send (channel full); woken (one at a time) when try_recv succeeds.
    pub send_waiters: WaiterQueue,
    /// Single task sleeping in sys_wait_for_event watching this channel; woken via CAS.
    pub event_waiter: EventWaiterSlot,
}

pub struct ChannelInner {
    pub queue: VecDeque<Vec<u8>>,
    pub capacity: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcError {
    InvalidEndpoint,
    WrongDirection,
    PeerClosed,
    ChannelFull,
    WouldBlock,
    MessageTooLarge,
    InvalidArgs,
}

pub fn create_channel(capacity: usize) -> (u64, u64) {
    let capacity = if capacity == 0 {
        DEFAULT_CHANNEL_CAPACITY
    } else {
        capacity.clamp(1, MAX_CHANNEL_CAPACITY)
    };

    let channel = Arc::new(Channel {
        inner: Mutex::new(ChannelInner {
            queue: VecDeque::new(),
            capacity,
        }),
        send_closed: AtomicBool::new(false),
        recv_closed: AtomicBool::new(false),
        send_count: AtomicU32::new(1),
        recv_count: AtomicU32::new(1),
        recv_waiters: Mutex::new(VecDeque::new()),
        send_waiters: Mutex::new(VecDeque::new()),
        event_waiter: Mutex::new(None),
    });

    let send_id = NEXT_ENDPOINT_ID.fetch_add(1, Ordering::Relaxed);
    let recv_id = NEXT_ENDPOINT_ID.fetch_add(1, Ordering::Relaxed);

    let send_ep = Endpoint {
        role: EndpointRole::Send,
        channel: channel.clone(),
        handle_refs: 0,
    };
    let recv_ep = Endpoint {
        role: EndpointRole::Recv,
        channel,
        handle_refs: 0,
    };

    let mut registry = ENDPOINT_REGISTRY.lock();
    registry.insert(send_id, send_ep);
    registry.insert(recv_id, recv_ep);

    (send_id, recv_id)
}

fn wake_waiter(waiters: &WaiterQueue) {
    if let Some((task, cpu_id)) = waiters.lock().pop_front() {
        task.state.store(TaskState::Ready, Ordering::Release);
        crate::task::local_scheduler::add_front(crate::memory::cpu_local_data::get_cpu(cpu_id), task);
        let local_kernel_id = crate::memory::cpu_local_data::get_local().kernel_id;
        if cpu_id != local_kernel_id {
            let apic_id = crate::memory::cpu_local_data::local_apic_id_of(cpu_id);
            crate::apic::send_fixed_ipi(apic_id, u8::from(crate::interrupt::InterruptVector::Reschedule));
        }
    }
}

pub fn try_send(endpoint_id: u64, data: &[u8]) -> Result<(), IpcError> {
    if data.len() > MAX_MESSAGE_SIZE {
        return Err(IpcError::MessageTooLarge);
    }

    let channel = {
        let registry = ENDPOINT_REGISTRY.lock();
        let ep = registry.get(&endpoint_id).ok_or(IpcError::InvalidEndpoint)?;
        if ep.role != EndpointRole::Send {
            return Err(IpcError::WrongDirection);
        }
        ep.channel.clone()
    };

    if channel.recv_closed.load(Ordering::Acquire) {
        return Err(IpcError::PeerClosed);
    }

    let mut inner = channel.inner.lock();
    if inner.queue.len() >= inner.capacity {
        return Err(IpcError::ChannelFull);
    }

    inner.queue.push_back(data.to_vec());
    drop(inner);
    // Wake any task that was sleeping waiting to receive
    wake_waiter(&channel.recv_waiters);
    // Wake any task sleeping in sys_wait_for_event watching this channel
    crate::task::local_scheduler::try_wake_slot(&channel.event_waiter);
    Ok(())
}

pub fn try_recv(endpoint_id: u64) -> Result<Vec<u8>, IpcError> {
    let channel = {
        let registry = ENDPOINT_REGISTRY.lock();
        let ep = registry.get(&endpoint_id).ok_or(IpcError::InvalidEndpoint)?;
        if ep.role != EndpointRole::Recv {
            return Err(IpcError::WrongDirection);
        }
        ep.channel.clone()
    };

    let mut inner = channel.inner.lock();
    if let Some(msg) = inner.queue.pop_front() {
        drop(inner);
        // Wake any task that was sleeping waiting to send (queue was full)
        wake_waiter(&channel.send_waiters);
        return Ok(msg);
    }

    if channel.send_closed.load(Ordering::Acquire) {
        return Err(IpcError::PeerClosed);
    }

    Err(IpcError::WouldBlock)
}

/// Non-consuming peek: returns true if the recv endpoint has at least one queued message.
pub fn channel_has_message(endpoint_id: u64) -> bool {
    let channel = {
        let registry = ENDPOINT_REGISTRY.lock();
        let ep = match registry.get(&endpoint_id) {
            Some(ep) if ep.role == EndpointRole::Recv => ep,
            _ => return false,
        };
        ep.channel.clone()
    };
    !channel.inner.lock().queue.is_empty()
}

/// Force-close an endpoint (used by `sys_channel_close`). Removes from registry
/// regardless of handle refcount. Only marks the channel side as closed when
/// no other endpoints of the same role remain.
pub fn close_endpoint(endpoint_id: u64) -> Result<(), IpcError> {
    let ep = {
        let mut registry = ENDPOINT_REGISTRY.lock();
        registry.remove(&endpoint_id).ok_or(IpcError::InvalidEndpoint)?
    };

    mark_side_closed_if_last(&ep);
    Ok(())
}

/// Clone an endpoint: create a new endpoint ID on the same channel with the
/// same role. The new endpoint is born with `handle_refs = 1`.
pub fn clone_endpoint(endpoint_id: u64) -> Result<u64, IpcError> {
    let (role, channel) = {
        let registry = ENDPOINT_REGISTRY.lock();
        let ep = registry.get(&endpoint_id).ok_or(IpcError::InvalidEndpoint)?;
        (ep.role, ep.channel.clone())
    };

    // Increment the endpoint count for this side of the channel.
    match role {
        EndpointRole::Send => { channel.send_count.fetch_add(1, Ordering::Relaxed); }
        EndpointRole::Recv => { channel.recv_count.fetch_add(1, Ordering::Relaxed); }
    }

    let new_id = NEXT_ENDPOINT_ID.fetch_add(1, Ordering::Relaxed);
    let new_ep = Endpoint {
        role,
        channel,
        handle_refs: 1,
    };

    let mut registry = ENDPOINT_REGISTRY.lock();
    registry.insert(new_id, new_ep);
    Ok(new_id)
}

/// Decrement the handle reference count. Only close the endpoint when it
/// reaches 0. Returns Ok(()) if the endpoint was released (or already gone).
pub fn release_endpoint(endpoint_id: u64) -> Result<(), IpcError> {
    let mut registry = ENDPOINT_REGISTRY.lock();
    let ep = match registry.get_mut(&endpoint_id) {
        Some(ep) => ep,
        None => return Ok(()), // already closed (e.g. by sys_channel_close)
    };

    if ep.handle_refs > 1 {
        ep.handle_refs -= 1;
        return Ok(());
    }

    // Last handle — remove and close.
    let ep = registry.remove(&endpoint_id).unwrap();
    drop(registry);

    mark_side_closed_if_last(&ep);
    Ok(())
}

/// Decrement the endpoint count for this side of the channel. If it reaches 0,
/// mark that side as closed (so the peer sees EOF / broken pipe).
fn mark_side_closed_if_last(ep: &Endpoint) {
    let prev = match ep.role {
        EndpointRole::Send => ep.channel.send_count.fetch_sub(1, Ordering::AcqRel),
        EndpointRole::Recv => ep.channel.recv_count.fetch_sub(1, Ordering::AcqRel),
    };
    // prev is the value *before* decrement. If it was 1, we just decremented to 0.
    if prev == 1 {
        match ep.role {
            EndpointRole::Send => ep.channel.send_closed.store(true, Ordering::Release),
            EndpointRole::Recv => ep.channel.recv_closed.store(true, Ordering::Release),
        }
    }
}
