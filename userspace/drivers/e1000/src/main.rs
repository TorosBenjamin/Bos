#![no_std]
#![no_main]

mod descriptors;
mod driver;
mod regs;

use driver::{E1000, MAX_FRAME};
use regs::{MSG_SUBSCRIBE, MSG_TX_PACKET, MSG_GET_MAC};
use ulib::{sys_channel_create, sys_register_service, sys_try_channel_recv, sys_try_channel_send, sys_yield};
use kernel_api_types::{IPC_OK, IPC_ERR_PEER_CLOSED};

/// Maximum number of tasks that can subscribe to receive packets.
const MAX_SUBSCRIBERS: usize = 4;

/// IPC message buffer: 1 byte type + 2 byte len + up to MAX_FRAME bytes.
const MSG_BUF_SIZE: usize = 3 + MAX_FRAME;

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point(_arg: u64) -> ! {
    let mut driver = match E1000::init() {
        Some(d) => d,
        None => {
            ulib::sys_debug_log(0x000E_1000_DEAD, 0xE1);
            ulib::sys_exit(1);
        }
    };

    ulib::sys_debug_log(
        u64::from_le_bytes([driver.mac[0], driver.mac[1], driver.mac[2],
                            driver.mac[3], driver.mac[4], driver.mac[5], 0, 0]),
        0xE1_0000,
    );

    // Create the service channel and register it.
    let (send_ep, recv_ep) = sys_channel_create(32);
    sys_register_service(b"e1000", send_ep);

    // Subscriber list: send endpoints to forward received packets to.
    let mut subscribers = [0u64; MAX_SUBSCRIBERS];

    // Reusable buffers.
    let mut msg_buf = [0u8; MSG_BUF_SIZE];
    let mut rx_buf  = [0u8; MAX_FRAME];

    loop {
        // ── 1. Forward any received Ethernet frames to subscribers ────────────
        while let Some(len) = driver.recv(&mut rx_buf) {
            if len < 14 {
                continue; // too short to be a valid Ethernet frame
            }
            // Build notification: [len: u16 LE][frame data]
            let notif_len = 2 + len;
            let mut notif = [0u8; 2 + MAX_FRAME];
            notif[0..2].copy_from_slice(&(len as u16).to_le_bytes());
            notif[2..2 + len].copy_from_slice(&rx_buf[..len]);

            for slot in &mut subscribers {
                if *slot == 0 {
                    continue;
                }
                let ret = sys_try_channel_send(*slot, &notif[..notif_len]);
                if ret == IPC_ERR_PEER_CLOSED {
                    *slot = 0; // subscriber disconnected, clear the slot
                }
                // IPC_ERR_CHANNEL_FULL → drop for this subscriber (best-effort)
            }
        }

        // ── 2. Drain incoming requests (TX packets / subscriptions) ──────────
        loop {
            let (ret, n) = sys_try_channel_recv(recv_ep, &mut msg_buf);
            if ret != IPC_OK {
                break;
            }
            if n == 0 {
                continue;
            }

            match msg_buf[0] {
                MSG_TX_PACKET if n >= 3 => {
                    let len = u16::from_le_bytes([msg_buf[1], msg_buf[2]]) as usize;
                    if len > 0 && len <= MAX_FRAME && 3 + len <= n as usize {
                        driver.send(&msg_buf[3..3 + len]);
                    }
                }
                MSG_SUBSCRIBE if n >= 9 => {
                    let ep = u64::from_le_bytes([
                        msg_buf[1], msg_buf[2], msg_buf[3], msg_buf[4],
                        msg_buf[5], msg_buf[6], msg_buf[7], msg_buf[8],
                    ]);
                    // Find a free slot.
                    let mut added = false;
                    for slot in &mut subscribers {
                        if *slot == 0 {
                            *slot = ep;
                            added = true;
                            break;
                        }
                    }
                    if !added {
                        ulib::sys_debug_log(ep, 0xE1_0001);
                    }
                }
                MSG_GET_MAC if n >= 9 => {
                    let reply_ep = u64::from_le_bytes([
                        msg_buf[1], msg_buf[2], msg_buf[3], msg_buf[4],
                        msg_buf[5], msg_buf[6], msg_buf[7], msg_buf[8],
                    ]);
                    sys_try_channel_send(reply_ep, &driver.mac);
                }
                _ => {} // unknown or malformed message — ignore
            }
        }

        sys_yield();
    }
}

#[panic_handler]
fn rust_panic(_info: &core::panic::PanicInfo) -> ! {
    ulib::sys_exit(1);
}
