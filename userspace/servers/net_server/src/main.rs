#![no_std]
#![no_main]

extern crate alloc;

mod device;

use alloc::vec::Vec;
use device::{E1000Client, MSG_SUBSCRIBE};
use kernel_api_types::SVC_ERR_NOT_FOUND;
use linked_list_allocator::LockedHeap;
use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, Ipv4Address};
use ulib::{sys_channel_create, sys_lookup_service, sys_register_service, sys_sleep_ms,
           sys_try_channel_send, sys_yield};

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

const HEAP_SIZE: usize = 512 * 1024;

static mut HEAP: [u8; HEAP_SIZE] = [0u8; HEAP_SIZE];

fn now() -> Instant {
    // sys_get_time_ns returns nanoseconds; smoltcp Instant works in milliseconds.
    Instant::from_millis((ulib::sys_get_time_ns() / 1_000_000) as i64)
}

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point(_arg: u64) -> ! {
    unsafe {
        let heap_start = core::ptr::addr_of_mut!(HEAP) as *mut u8;
        ALLOCATOR.lock().init(heap_start, HEAP_SIZE);
    }

    // Wait for the e1000 driver to register itself.
    let e1000_ep = loop {
        let ep = sys_lookup_service(b"e1000");
        if ep != SVC_ERR_NOT_FOUND {
            break ep;
        }
        sys_sleep_ms(10);
    };

    // Create a channel so e1000 can push received frames to us.
    let (rx_notify_send_ep, rx_notify_recv_ep) = sys_channel_create(64);

    // Subscribe to the e1000 driver for RX notifications.
    let mut sub = [0u8; 9];
    sub[0] = MSG_SUBSCRIBE;
    sub[1..9].copy_from_slice(&rx_notify_send_ep.to_le_bytes());
    sys_try_channel_send(e1000_ep, &sub);

    // QEMU's default MAC for the first e1000 device.
    // TODO: expose MAC via e1000 IPC instead of hardcoding.
    let mac = EthernetAddress([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);

    let mut device = E1000Client::new(e1000_ep, rx_notify_recv_ep);

    // Use the current time as a random seed so TCP ISNs differ between boots.
    let mut config = Config::new(HardwareAddress::Ethernet(mac));
    config.random_seed = ulib::sys_get_time_ns();

    let mut iface = Interface::new(config, &mut device, now());

    // QEMU user-mode networking: guest is 10.0.2.15/24, gateway 10.0.2.2.
    iface.update_ip_addrs(|addrs| {
        let _ = addrs.push(IpCidr::new(IpAddress::v4(10, 0, 2, 15), 24));
    });
    iface
        .routes_mut()
        .add_default_ipv4_route(Ipv4Address::new(10, 0, 2, 2))
        .unwrap();

    let mut sockets: SocketSet<'static> = SocketSet::new(Vec::new());

    // Register this server so other tasks can request sockets.
    // TODO: define the socket-request IPC protocol and serve it here.
    let (net_send_ep, _net_recv_ep) = sys_channel_create(32);
    sys_register_service(b"net", net_send_ep);

    loop {
        iface.poll(now(), &mut device, &mut sockets);
        // TODO: drain _net_recv_ep for socket open/close/send/recv requests.
        sys_yield();
    }
}

#[panic_handler]
fn rust_panic(_info: &core::panic::PanicInfo) -> ! {
    ulib::sys_exit(1);
}
