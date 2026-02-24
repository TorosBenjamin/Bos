use alloc::collections::BTreeMap;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use spin::Mutex;
use crate::task::task::{Task, TaskState};

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
}

pub struct Channel {
    pub inner: Mutex<ChannelInner>,
    pub send_closed: AtomicBool,
    pub recv_closed: AtomicBool,
    /// Task sleeping waiting to receive; woken when try_send succeeds.
    pub recv_waiter: Mutex<Option<(Arc<Task>, u32)>>,
    /// Task sleeping waiting to send (channel full); woken when try_recv succeeds.
    pub send_waiter: Mutex<Option<(Arc<Task>, u32)>>,
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
        recv_waiter: Mutex::new(None),
        send_waiter: Mutex::new(None),
    });

    let send_id = NEXT_ENDPOINT_ID.fetch_add(1, Ordering::Relaxed);
    let recv_id = NEXT_ENDPOINT_ID.fetch_add(1, Ordering::Relaxed);

    let send_ep = Endpoint {
        role: EndpointRole::Send,
        channel: channel.clone(),
    };
    let recv_ep = Endpoint {
        role: EndpointRole::Recv,
        channel,
    };

    let mut registry = ENDPOINT_REGISTRY.lock();
    registry.insert(send_id, send_ep);
    registry.insert(recv_id, recv_ep);

    (send_id, recv_id)
}

fn wake_waiter(waiter: &Mutex<Option<(Arc<Task>, u32)>>) {
    if let Some((task, cpu_id)) = waiter.lock().take() {
        task.state.store(TaskState::Ready, Ordering::Release);
        crate::task::local_scheduler::add(crate::memory::cpu_local_data::get_cpu(cpu_id), task);
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
    wake_waiter(&channel.recv_waiter);
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
        wake_waiter(&channel.send_waiter);
        return Ok(msg);
    }

    if channel.send_closed.load(Ordering::Acquire) {
        return Err(IpcError::PeerClosed);
    }

    Err(IpcError::WouldBlock)
}

pub fn close_endpoint(endpoint_id: u64) -> Result<(), IpcError> {
    let ep = {
        let mut registry = ENDPOINT_REGISTRY.lock();
        registry.remove(&endpoint_id).ok_or(IpcError::InvalidEndpoint)?
    };

    match ep.role {
        EndpointRole::Send => ep.channel.send_closed.store(true, Ordering::Release),
        EndpointRole::Recv => ep.channel.recv_closed.store(true, Ordering::Release),
    }

    Ok(())
}
