use crate::TestResult;
use alloc::format;
use kernel::ipc;

pub fn test_channel_create() -> TestResult {
    let (send_id, recv_id) = ipc::create_channel(16);
    if send_id == 0 {
        return TestResult::Failed("send_id is 0".into());
    }
    if recv_id == 0 {
        return TestResult::Failed("recv_id is 0".into());
    }
    if send_id == recv_id {
        return TestResult::Failed("send_id == recv_id".into());
    }
    // Clean up
    let _ = ipc::close_endpoint(send_id);
    let _ = ipc::close_endpoint(recv_id);
    TestResult::Ok
}

pub fn test_send_recv() -> TestResult {
    let (send_id, recv_id) = ipc::create_channel(16);
    let msg = b"Hello, IPC!";

    if let Err(e) = ipc::try_send(send_id, msg) {
        let _ = ipc::close_endpoint(send_id);
        let _ = ipc::close_endpoint(recv_id);
        return TestResult::Failed(format!("try_send failed: {:?}", e));
    }

    match ipc::try_recv(recv_id) {
        Ok(data) => {
            let _ = ipc::close_endpoint(send_id);
            let _ = ipc::close_endpoint(recv_id);
            if data.as_slice() != msg {
                return TestResult::Failed(format!(
                    "Data mismatch: expected {:?}, got {:?}",
                    msg, data
                ));
            }
            TestResult::Ok
        }
        Err(e) => {
            let _ = ipc::close_endpoint(send_id);
            let _ = ipc::close_endpoint(recv_id);
            TestResult::Failed(format!("try_recv failed: {:?}", e))
        }
    }
}

pub fn test_send_on_recv_fails() -> TestResult {
    let (send_id, recv_id) = ipc::create_channel(16);

    match ipc::try_send(recv_id, b"nope") {
        Err(ipc::IpcError::WrongDirection) => {}
        other => {
            let _ = ipc::close_endpoint(send_id);
            let _ = ipc::close_endpoint(recv_id);
            return TestResult::Failed(format!("Expected WrongDirection, got {:?}", other));
        }
    }

    let _ = ipc::close_endpoint(send_id);
    let _ = ipc::close_endpoint(recv_id);
    TestResult::Ok
}

pub fn test_recv_on_send_fails() -> TestResult {
    let (send_id, recv_id) = ipc::create_channel(16);

    match ipc::try_recv(send_id) {
        Err(ipc::IpcError::WrongDirection) => {}
        other => {
            let _ = ipc::close_endpoint(send_id);
            let _ = ipc::close_endpoint(recv_id);
            return TestResult::Failed(format!("Expected WrongDirection, got {:?}", other));
        }
    }

    let _ = ipc::close_endpoint(send_id);
    let _ = ipc::close_endpoint(recv_id);
    TestResult::Ok
}

pub fn test_close_then_fail() -> TestResult {
    let (send_id, recv_id) = ipc::create_channel(16);

    // Close the send side
    if let Err(e) = ipc::close_endpoint(send_id) {
        let _ = ipc::close_endpoint(recv_id);
        return TestResult::Failed(format!("close_endpoint failed: {:?}", e));
    }

    // Sending on a closed endpoint should return InvalidEndpoint
    match ipc::try_send(send_id, b"data") {
        Err(ipc::IpcError::InvalidEndpoint) => {}
        other => {
            let _ = ipc::close_endpoint(recv_id);
            return TestResult::Failed(format!(
                "Expected InvalidEndpoint on closed send, got {:?}",
                other
            ));
        }
    }

    // Receiving when sender is closed and queue is empty should return PeerClosed
    match ipc::try_recv(recv_id) {
        Err(ipc::IpcError::PeerClosed) => {}
        other => {
            let _ = ipc::close_endpoint(recv_id);
            return TestResult::Failed(format!(
                "Expected PeerClosed on recv after send closed, got {:?}",
                other
            ));
        }
    }

    let _ = ipc::close_endpoint(recv_id);
    TestResult::Ok
}

pub fn test_channel_full() -> TestResult {
    let cap = 4;
    let (send_id, recv_id) = ipc::create_channel(cap);

    // Fill to capacity
    for i in 0..cap {
        if let Err(e) = ipc::try_send(send_id, &[i as u8]) {
            let _ = ipc::close_endpoint(send_id);
            let _ = ipc::close_endpoint(recv_id);
            return TestResult::Failed(format!("try_send #{} failed: {:?}", i, e));
        }
    }

    // Next send should fail with ChannelFull
    match ipc::try_send(send_id, &[0xFF]) {
        Err(ipc::IpcError::ChannelFull) => {}
        other => {
            let _ = ipc::close_endpoint(send_id);
            let _ = ipc::close_endpoint(recv_id);
            return TestResult::Failed(format!("Expected ChannelFull, got {:?}", other));
        }
    }

    let _ = ipc::close_endpoint(send_id);
    let _ = ipc::close_endpoint(recv_id);
    TestResult::Ok
}

pub fn test_recv_closed_then_send_fails() -> TestResult {
    let (send_id, recv_id) = ipc::create_channel(16);

    // Close the recv side
    if let Err(e) = ipc::close_endpoint(recv_id) {
        let _ = ipc::close_endpoint(send_id);
        return TestResult::Failed(format!("close_endpoint failed: {:?}", e));
    }

    // Sending when receiver is closed should return PeerClosed
    match ipc::try_send(send_id, b"data") {
        Err(ipc::IpcError::PeerClosed) => {}
        other => {
            let _ = ipc::close_endpoint(send_id);
            return TestResult::Failed(format!("Expected PeerClosed, got {:?}", other));
        }
    }

    let _ = ipc::close_endpoint(send_id);
    TestResult::Ok
}

pub fn test_fifo_order() -> TestResult {
    let (send_id, recv_id) = ipc::create_channel(16);

    for i in 0u8..5 {
        if let Err(e) = ipc::try_send(send_id, &[i, i + 10, i + 20]) {
            let _ = ipc::close_endpoint(send_id);
            let _ = ipc::close_endpoint(recv_id);
            return TestResult::Failed(format!("try_send #{} failed: {:?}", i, e));
        }
    }

    for i in 0u8..5 {
        match ipc::try_recv(recv_id) {
            Ok(data) => {
                let expected = [i, i + 10, i + 20];
                if data.as_slice() != expected {
                    let _ = ipc::close_endpoint(send_id);
                    let _ = ipc::close_endpoint(recv_id);
                    return TestResult::Failed(format!(
                        "Message #{}: expected {:?}, got {:?}",
                        i, expected, data
                    ));
                }
            }
            Err(e) => {
                let _ = ipc::close_endpoint(send_id);
                let _ = ipc::close_endpoint(recv_id);
                return TestResult::Failed(format!("try_recv #{} failed: {:?}", i, e));
            }
        }
    }

    let _ = ipc::close_endpoint(send_id);
    let _ = ipc::close_endpoint(recv_id);
    TestResult::Ok
}
