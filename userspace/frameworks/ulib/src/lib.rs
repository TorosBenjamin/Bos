#![no_std]

pub mod display;
pub mod fs;
pub mod net;
pub mod window;
pub mod test_framework;

use core::arch::asm;
use kernel_api_types::{SysCallNumber, SVC_ERR_NOT_FOUND, SVC_OK};
pub use kernel_api_types::{WAIT_KEYBOARD, WAIT_MOUSE};
use kernel_api_types::graphics::{DisplayInfo, GraphicsResult, Rect};

pub fn syscall(inputs_and_ouputs: &mut [u64; 7]) {
    unsafe {
        asm!("
            syscall
            ",
        inlateout("rdi") inputs_and_ouputs[0],
        inlateout("rsi") inputs_and_ouputs[1],
        inlateout("rdx") inputs_and_ouputs[2],
        inlateout("r10") inputs_and_ouputs[3],
        inlateout("r8") inputs_and_ouputs[4],
        inlateout("r9") inputs_and_ouputs[5],
        inlateout("rax") inputs_and_ouputs[6],
        );
    }
}

pub fn sys_get_bounding_box(out_rect: &mut Rect) -> GraphicsResult {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::GetBoundingBox as u64;
    args[1] = out_rect as *const Rect as u64;

    syscall(&mut args);

    let ret = args[6];
    GraphicsResult::from_u64(ret)
}

pub fn sys_get_display_info() -> DisplayInfo {
    let mut info = DisplayInfo {
        width: 0,
        height: 0,
        red_mask_size: 0,
        red_mask_shift: 0,
        green_mask_size: 0,
        green_mask_shift: 0,
        blue_mask_size: 0,
        blue_mask_shift: 0,
        pitch: 0,
    };
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::GetDisplayInfo as u64;
    args[1] = &mut info as *mut DisplayInfo as u64;

    syscall(&mut args);

    info
}

pub fn sys_read_mouse() -> Option<kernel_api_types::MouseEvent> {
    let mut event = kernel_api_types::MouseEvent::EMPTY;
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::ReadMouse as u64;
    args[1] = &mut event as *mut kernel_api_types::MouseEvent as u64;

    syscall(&mut args);

    if args[6] == 0 { Some(event) } else { None }
}

pub fn sys_read_key() -> kernel_api_types::KeyEvent {
    let mut event = kernel_api_types::KeyEvent::EMPTY;
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::ReadKey as u64;
    args[1] = &mut event as *mut kernel_api_types::KeyEvent as u64;

    syscall(&mut args);

    event
}

pub fn sys_try_read_key() -> Option<kernel_api_types::KeyEvent> {
    let mut event = kernel_api_types::KeyEvent::EMPTY;
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::TryReadKey as u64;
    args[1] = &mut event as *mut kernel_api_types::KeyEvent as u64;

    syscall(&mut args);

    if args[6] == 0 { Some(event) } else { None }
}

/// Non-blocking channel send. Returns IPC_OK if enqueued, IPC_ERR_CHANNEL_FULL if full.
/// Never blocks or sleeps.
pub fn sys_try_channel_send(endpoint_id: u64, data: &[u8]) -> u64 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::TryChannelSend as u64;
    args[1] = endpoint_id;
    args[2] = data.as_ptr() as u64;
    args[3] = data.len() as u64;
    syscall(&mut args);
    args[6]
}

pub fn sys_try_channel_recv(endpoint_id: u64, buf: &mut [u8]) -> (u64, u64) {
    let mut bytes_read: u64 = 0;
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::TryChannelRecv as u64;
    args[1] = endpoint_id;
    args[2] = buf.as_mut_ptr() as u64;
    args[3] = buf.len() as u64;
    args[4] = &mut bytes_read as *mut u64 as u64;
    syscall(&mut args);
    (args[6], bytes_read)
}

pub fn sys_yield() {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::Yield as u64;
    syscall(&mut args);
}

/// Sleep for at least `ms` milliseconds.
pub fn sys_sleep_ms(ms: u64) {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::SleepMs as u64;
    args[1] = ms;
    syscall(&mut args);
}

pub fn sys_mmap(size: u64, flags: u64) -> *mut u8 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::Mmap as u64;
    args[1] = size;
    args[2] = flags;
    syscall(&mut args);
    args[6] as *mut u8
}

pub fn sys_munmap(addr: *mut u8, size: u64) -> u64 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::Munmap as u64;
    args[1] = addr as u64;
    args[2] = size;
    syscall(&mut args);
    args[6]
}

/// Change protection flags on [addr, addr+size). Flags: MMAP_WRITE, MMAP_EXEC.
/// Returns 0 on success, !0 on error.
pub fn sys_mprotect(addr: *mut u8, size: u64, flags: u64) -> u64 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::Mprotect as u64;
    args[1] = addr as u64;
    args[2] = size;
    args[3] = flags;
    syscall(&mut args);
    args[6]
}

/// Resize an mmap allocation. flags: MREMAP_MAYMOVE.
/// Returns new address on success (may equal addr), null on error.
pub fn sys_mremap(addr: *mut u8, old_size: u64, new_size: u64, flags: u64) -> *mut u8 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::Mremap as u64;
    args[1] = addr as u64;
    args[2] = old_size;
    args[3] = new_size;
    args[4] = flags;
    syscall(&mut args);
    args[6] as *mut u8
}

pub fn sys_spawn_named(elf_bytes: &[u8], child_arg: u64, name: &[u8]) -> u64 {
    sys_spawn_with_priority(elf_bytes, child_arg, name, kernel_api_types::Priority::Normal as u8)
}

/// Spawn a new task with an explicit priority request (clamped to parent priority by kernel).
pub fn sys_spawn_with_priority(elf_bytes: &[u8], child_arg: u64, name: &[u8], priority: u8) -> u64 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::Spawn as u64;
    args[1] = elf_bytes.as_ptr() as u64;
    args[2] = elf_bytes.len() as u64;
    args[3] = child_arg;
    args[4] = name.as_ptr() as u64;
    args[5] = name.len() as u64;
    args[6] = priority as u64;
    syscall(&mut args);
    args[6]
}

/// Lower the calling task's priority (kernel ignores if requested >= current).
/// Returns 0 if lowered, 1 if already at or below requested level.
pub fn sys_set_priority(priority: u8) -> u64 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::SetPriority as u64;
    args[1] = priority as u64;
    syscall(&mut args);
    args[6]
}

pub fn sys_spawn(elf_bytes: &[u8], child_arg: u64) -> u64 {
    sys_spawn_named(elf_bytes, child_arg, b"")
}

pub fn sys_channel_create(capacity: u64) -> (u64, u64) {
    let mut send_ep: u64 = 0;
    let mut recv_ep: u64 = 0;
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::ChannelCreate as u64;
    args[1] = &mut send_ep as *mut u64 as u64;
    args[2] = &mut recv_ep as *mut u64 as u64;
    args[3] = capacity;
    syscall(&mut args);
    (send_ep, recv_ep)
}

pub fn sys_channel_send(endpoint_id: u64, data: &[u8]) -> u64 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::ChannelSend as u64;
    args[1] = endpoint_id;
    args[2] = data.as_ptr() as u64;
    args[3] = data.len() as u64;
    syscall(&mut args);
    args[6]
}

pub fn sys_channel_recv(endpoint_id: u64, buf: &mut [u8]) -> (u64, u64) {
    let mut bytes_read: u64 = 0;
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::ChannelRecv as u64;
    args[1] = endpoint_id;
    args[2] = buf.as_mut_ptr() as u64;
    args[3] = buf.len() as u64;
    args[4] = &mut bytes_read as *mut u64 as u64;
    syscall(&mut args);
    (args[6], bytes_read)
}

pub fn sys_channel_close(endpoint_id: u64) -> u64 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::ChannelClose as u64;
    args[1] = endpoint_id;
    syscall(&mut args);
    args[6]
}

/// Block until the target task has finished loading its ELF (leaves Loading state).
/// Returns 0 on success, 1 if task not found, 2 if load failed.
pub fn sys_wait_task_ready(task_id: u64) -> u64 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::WaitTaskReady as u64;
    args[1] = task_id;
    syscall(&mut args);
    args[6]
}

pub fn sys_transfer_display(new_owner_task_id: u64) -> u64 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::TransferDisplay as u64;
    args[1] = new_owner_task_id;
    syscall(&mut args);
    args[6]
}

/// Emit a debug value to the kernel serial console.
/// `tag` is a u64 label printed alongside `value`.
pub fn sys_debug_log(value: u64, tag: u64) {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::DebugLog as u64;
    args[1] = value;
    args[2] = tag;
    syscall(&mut args);
}

pub fn sys_get_module(name: &str, buf: *mut u8, buf_cap: u64) -> u64 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::GetModule as u64;
    args[1] = name.as_ptr() as u64;
    args[2] = name.len() as u64;
    args[3] = buf as u64;
    args[4] = buf_cap;
    syscall(&mut args);
    args[6]
}

/// Register a send endpoint under a human-readable service name.
/// Returns `SVC_OK` on success or a `SVC_ERR_*` code on failure.
pub fn sys_register_service(name: &[u8], send_ep: u64) -> u64 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::RegisterService as u64;
    args[1] = name.as_ptr() as u64;
    args[2] = name.len() as u64;
    args[3] = send_ep;
    syscall(&mut args);
    args[6]
}

/// Look up a service by name.
/// Returns the send endpoint ID on success, or `SVC_ERR_NOT_FOUND` if not yet registered.
pub fn sys_lookup_service(name: &[u8]) -> u64 {
    let mut ep_out: u64 = 0;
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::LookupService as u64;
    args[1] = name.as_ptr() as u64;
    args[2] = name.len() as u64;
    args[3] = &mut ep_out as *mut u64 as u64;
    syscall(&mut args);
    if args[6] == SVC_OK {
        ep_out
    } else {
        SVC_ERR_NOT_FOUND
    }
}

/// Allocate a shared physical buffer of `size` bytes and map it into the caller's address space.
/// Returns `(shared_buf_id, ptr)`. On failure `shared_buf_id` is `u64::MAX` and `ptr` is null.
pub fn sys_create_shared_buf(size: u64) -> (u64, *mut u8) {
    let mut vaddr_out: u64 = 0;
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::CreateSharedBuf as u64;
    args[1] = size;
    args[2] = &mut vaddr_out as *mut u64 as u64;
    syscall(&mut args);
    (args[6], vaddr_out as *mut u8)
}

/// Map a shared buffer (created by another task) into the caller's address space.
/// Returns a writable pointer to the region, or null on failure.
pub fn sys_map_shared_buf(id: u64) -> *mut u8 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::MapSharedBuf as u64;
    args[1] = id;
    syscall(&mut args);
    args[6] as *mut u8
}

/// Free the physical pages backing a shared buffer.
/// Should be called by the creator after all other mappings have been removed.
pub fn sys_destroy_shared_buf(id: u64) {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::DestroySharedBuf as u64;
    args[1] = id;
    syscall(&mut args);
}

pub fn sys_shutdown(exit_code: u64) -> ! {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::Shutdown as u64;
    args[1] = exit_code;
    syscall(&mut args);
    loop {
        core::hint::spin_loop();
    }
}

/// Read from an x86 I/O port. `width`: 1=byte, 2=word, 4=dword.
pub fn sys_ioport_read(port: u16, width: u8) -> u64 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::IoPortRead as u64;
    args[1] = port as u64;
    args[2] = width as u64;
    syscall(&mut args);
    args[6]
}

/// Write to an x86 I/O port. `width`: 1=byte, 2=word, 4=dword.
pub fn sys_ioport_write(port: u16, value: u32, width: u8) {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::IoPortWrite as u64;
    args[1] = port as u64;
    args[2] = value as u64;
    args[3] = width as u64;
    syscall(&mut args);
}

#[inline] pub fn inb(port: u16) -> u8 { sys_ioport_read(port, 1) as u8 }
#[inline] pub fn inw(port: u16) -> u16 { sys_ioport_read(port, 2) as u16 }
#[inline] pub fn ind(port: u16) -> u32 { sys_ioport_read(port, 4) as u32 }
#[inline] pub fn outb(port: u16, val: u8) { sys_ioport_write(port, val as u32, 1); }
#[inline] pub fn outw(port: u16, val: u16) { sys_ioport_write(port, val as u32, 2); }
#[inline] pub fn outd(port: u16, val: u32) { sys_ioport_write(port, val, 4); }

/// Create a new thread sharing the caller's address space.
/// `entry` is the function pointer, `stack_top` is the top of an already-allocated stack,
/// and `arg` is passed in RDI on entry.
/// Returns the thread's task ID on success, or 0 on failure.
pub fn sys_thread_create(entry: u64, stack_top: u64, arg: u64) -> u64 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::ThreadCreate as u64;
    args[1] = entry;
    args[2] = stack_top;
    args[3] = arg;
    syscall(&mut args);
    args[6]
}

/// Register `send_ep` to receive a [`kernel_api_types::FaultEvent`] when `task_id`
/// is killed by a hardware fault (page fault, GPF, divide-by-zero).
/// Returns 0 on success, 1 on error (task not found or invalid endpoint).
pub fn sys_set_fault_ep(task_id: u64, send_ep: u64) -> u64 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::SetFaultEp as u64;
    args[1] = task_id;
    args[2] = send_ep;
    syscall(&mut args);
    args[6]
}

/// Register `send_ep` as the exit-notification endpoint for `task_id`.
/// Returns 0 on success, 1 on error (task not found or invalid endpoint).
pub fn sys_set_exit_channel(task_id: u64, send_ep: u64) -> u64 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::SetExitChannel as u64;
    args[1] = task_id;
    args[2] = send_ep;
    syscall(&mut args);
    args[6]
}


/// Read from PCI configuration space.
/// Returns the value (zero-extended to u32), or `None` on invalid arguments.
pub fn pci_config_read(bus: u8, device: u8, function: u8, offset: u8, width: u8) -> Option<u32> {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::PciConfigRead as u64;
    args[1] = bus as u64;
    args[2] = device as u64;
    args[3] = function as u64;
    args[4] = offset as u64;
    args[5] = width as u64;
    syscall(&mut args);
    if args[6] == u64::MAX {
        None
    } else {
        Some(args[6] as u32)
    }
}

/// Allocate one physically-backed 4 KiB page and return both its virtual address
/// and physical address. Intended for DMA-capable drivers.
///
/// Returns null on failure. On success, writes the physical address to `*phys_out`.
pub fn sys_alloc_dma(phys_out: &mut u64) -> *mut u8 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::AllocDma as u64;
    args[1] = 4096;
    args[2] = phys_out as *mut u64 as u64;
    syscall(&mut args);
    args[6] as *mut u8
}

/// Map a PCI device's MMIO BAR into the calling task's address space.
///
/// Returns a pointer to the mapped region on success, or null on failure.
/// The region is mapped uncached (NO_CACHE | WRITE_THROUGH | NO_EXECUTE).
pub fn sys_map_pci_bar(bus: u8, device: u8, function: u8, bar_index: u8) -> *mut u8 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::MapPciBar as u64;
    args[1] = bus as u64;
    args[2] = device as u64;
    args[3] = function as u64;
    args[4] = bar_index as u64;
    syscall(&mut args);
    args[6] as *mut u8
}

/// Write to PCI configuration space.
/// Returns `true` on success, `false` on invalid arguments.
pub fn pci_config_write(bus: u8, device: u8, function: u8, offset: u8, width: u8, value: u32) -> bool {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::PciConfigWrite as u64;
    args[1] = bus as u64;
    args[2] = device as u64;
    args[3] = function as u64;
    args[4] = offset as u64;
    args[5] = width as u64;
    args[6] = value as u64;
    syscall(&mut args);
    args[6] == 0
}

/// Block until any of the watched channels, mouse, or keyboard has data, or a timeout expires.
///
/// - `channels`: recv endpoint IDs to watch for incoming messages
/// - `flags`: `WAIT_KEYBOARD | WAIT_MOUSE` bitfield
/// - `timeout_ms`: 0 = infinite; non-zero = maximum wait in milliseconds
///
/// Returns: 0 = event available, 1 = timed out, 2 = invalid args
pub fn sys_wait_for_event(channels: &[u64], flags: u32, timeout_ms: u64) -> u64 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::WaitForEvent as u64;
    args[1] = channels.as_ptr() as u64;
    args[2] = channels.len() as u64;
    args[3] = flags as u64;
    args[4] = timeout_ms;
    syscall(&mut args);
    args[6]
}

/// Returns nanoseconds since the Unix epoch (wall-clock time, TSC-based).
pub fn sys_get_time_ns() -> u64 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::GetTimeNs as u64;
    syscall(&mut args);
    args[6]
}

/// Return the global timer tick count (monotonic, incremented ~once per ms).
pub fn sys_get_ticks() -> u64 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::GetTicks as u64;
    syscall(&mut args);
    args[6]
}

/// Return the cpu_ticks counter for a given task.
/// Returns u64::MAX if the task is not found.
pub fn sys_get_task_cpu_ticks(task_id: u64) -> u64 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::GetTaskCpuTicks as u64;
    args[1] = task_id;
    syscall(&mut args);
    args[6]
}

/// Terminate the calling task with exit code 1.
pub fn sys_exit(code: u64) -> ! {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::Exit as u64;
    args[1] = code;
    syscall(&mut args);
    loop { core::hint::spin_loop(); }
}

/// Default panic handler: exit the task so the rest of the system keeps running.
pub fn default_panic(_info: &core::panic::PanicInfo) -> ! {
    sys_exit(1);
}
