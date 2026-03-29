#![no_std]
#![no_main]

mod descriptors;
mod driver;
mod regs;

use driver::{E1000, MAX_FRAME};
use regs::{MSG_SUBSCRIBE, MSG_TX_PACKET, MSG_GET_MAC};
use ulib::sys_yield;

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
    let (send_fd, recv_fd) = ulib::handle::channel(32).unwrap();
    ulib::handle::register_service(b"e1000", send_fd);

    // Subscriber list: handle fds to forward received packets to.
    let mut subscribers = [0u32; MAX_SUBSCRIBERS];

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
                if ulib::handle::try_write(*slot, &notif[..notif_len]).is_none() {
                    // Peer closed or error — clear the slot.
                    ulib::handle::close(*slot);
                    *slot = 0;
                }
                // Would-block (returns Some(0)) → drop for this subscriber (best-effort)
            }
        }

        // ── 2. Drain incoming requests (TX packets / subscriptions) ──────────
        loop {
            let n = match ulib::handle::try_read(recv_fd, &mut msg_buf) {
                Some(n) if n > 0 => n,
                Some(_) => continue,
                None => break,
            };

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
                    // Wrap the endpoint as a send handle.
                    if let Some(fd) = ulib::handle::handle_from_channel(ep, 1) {
                        // Find a free slot.
                        let mut added = false;
                        for slot in &mut subscribers {
                            if *slot == 0 {
                                *slot = fd;
                                added = true;
                                break;
                            }
                        }
                        if !added {
                            ulib::handle::close(fd);
                            ulib::sys_debug_log(ep, 0xE1_0001);
                        }
                    }
                }
                MSG_GET_MAC if n >= 9 => {
                    let reply_ep = u64::from_le_bytes([
                        msg_buf[1], msg_buf[2], msg_buf[3], msg_buf[4],
                        msg_buf[5], msg_buf[6], msg_buf[7], msg_buf[8],
                    ]);
                    if let Some(fd) = ulib::handle::handle_from_channel(reply_ep, 1) {
                        ulib::handle::write(fd, &driver.mac);
                        ulib::handle::close(fd);
                    }
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
