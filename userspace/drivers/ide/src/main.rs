#![no_std]
#![no_main]

mod driver;

use driver::IdeDriver;
use ulib::{sys_channel_create, sys_register_service, sys_channel_recv, sys_channel_send,
           sys_channel_close, sys_create_shared_buf, sys_map_shared_buf, sys_destroy_shared_buf,
           sys_debug_log};
use kernel_api_types::IPC_OK;

// IPC message types
const MSG_READ: u8 = 1;
const MSG_WRITE: u8 = 2;
const MSG_READ_RESP: u8 = 3;
const MSG_WRITE_RESP: u8 = 4;

/// Maximum IPC message size.
const MAX_MSG: usize = 1024;

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point(_arg: u64) -> ! {
    let driver = match IdeDriver::init() {
        Some(d) => d,
        None => {
            sys_debug_log(0x1DE_DEAD, 0x1DE);
            ulib::sys_exit(1);
        }
    };

    sys_debug_log(driver.sector_count, 0x1DE_0000); // "ide: sectors"

    let (send_ep, recv_ep) = sys_channel_create(16);
    sys_register_service(b"ide", send_ep);

    let mut msg_buf = [0u8; MAX_MSG];

    loop {
        let (result, n) = sys_channel_recv(recv_ep, &mut msg_buf);
        if result != IPC_OK || n == 0 {
            continue;
        }
        let n = n as usize;

        match msg_buf[0] {
            MSG_READ  if n >= 21 => handle_read(&driver, &msg_buf[..n]),
            MSG_WRITE if n >= 29 => handle_write(&driver, &msg_buf[..n]),
            _ => {}
        }
    }
}

/// Read request: [1:type][8:lba LE][4:count LE][8:reply_ep LE] = 21 bytes
fn handle_read(driver: &IdeDriver, msg: &[u8]) {
    let lba = u64::from_le_bytes(msg[1..9].try_into().unwrap());
    let count = u32::from_le_bytes(msg[9..13].try_into().unwrap());
    let reply_ep = u64::from_le_bytes(msg[13..21].try_into().unwrap());

    if count == 0 || count > 256 {
        send_read_error(reply_ep);
        return;
    }

    if count == 1 {
        // Single sector: inline response [1:type][1:result][512:data] = 514 bytes
        let mut resp = [0u8; 514];
        resp[0] = MSG_READ_RESP;
        if driver.read_sectors(lba, 1, &mut resp[2..514]) {
            resp[1] = 0; // success
        } else {
            resp[1] = 1; // error
        }
        let _ = sys_channel_send(reply_ep, &resp);
        sys_channel_close(reply_ep);
    } else {
        // Multi-sector: shared buffer response
        let byte_count = count as u64 * 512;
        let (buf_id, ptr) = sys_create_shared_buf(byte_count);
        if ptr.is_null() || buf_id == u64::MAX {
            send_read_error(reply_ep);
            return;
        }

        let buf = unsafe { core::slice::from_raw_parts_mut(ptr, byte_count as usize) };
        if !driver.read_sectors(lba, count, buf) {
            sys_destroy_shared_buf(buf_id);
            send_read_error(reply_ep);
            return;
        }

        // Response: [1:type][1:result][8:shared_buf_id LE][4:byte_count LE] = 14 bytes
        let mut resp = [0u8; 14];
        resp[0] = MSG_READ_RESP;
        resp[1] = 0; // success
        resp[2..10].copy_from_slice(&buf_id.to_le_bytes());
        resp[10..14].copy_from_slice(&(byte_count as u32).to_le_bytes());
        let _ = sys_channel_send(reply_ep, &resp);
        sys_channel_close(reply_ep);
    }
}

fn send_read_error(reply_ep: u64) {
    let resp = [MSG_READ_RESP, 1]; // error
    let _ = sys_channel_send(reply_ep, &resp);
    sys_channel_close(reply_ep);
}

/// Write request: [1:type][8:lba LE][4:count LE][8:shared_buf_id LE][8:reply_ep LE] = 29 bytes
fn handle_write(driver: &IdeDriver, msg: &[u8]) {
    let lba = u64::from_le_bytes(msg[1..9].try_into().unwrap());
    let count = u32::from_le_bytes(msg[9..13].try_into().unwrap());
    let buf_id = u64::from_le_bytes(msg[13..21].try_into().unwrap());
    let reply_ep = u64::from_le_bytes(msg[21..29].try_into().unwrap());

    if count == 0 || count > 256 {
        send_write_resp(reply_ep, 1);
        return;
    }

    let byte_count = count as usize * 512;
    let ptr = sys_map_shared_buf(buf_id);
    if ptr.is_null() {
        send_write_resp(reply_ep, 1);
        return;
    }

    let buf = unsafe { core::slice::from_raw_parts(ptr as *const u8, byte_count) };
    let result = if driver.write_sectors(lba, count, buf) { 0u8 } else { 1u8 };

    // Unmap our mapping (munmap), but don't destroy — the caller owns the shared buf
    ulib::sys_munmap(ptr, byte_count as u64);

    send_write_resp(reply_ep, result);
}

fn send_write_resp(reply_ep: u64, result: u8) {
    let resp = [MSG_WRITE_RESP, result];
    let _ = sys_channel_send(reply_ep, &resp);
    sys_channel_close(reply_ep);
}

#[panic_handler]
fn rust_panic(_info: &core::panic::PanicInfo) -> ! {
    ulib::sys_exit(1);
}
