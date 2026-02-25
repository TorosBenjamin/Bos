use crate::TestResult;
use alloc::format;
use kernel::drivers::keyboard;
use kernel_api_types::{KeyEvent, KeyEventType};

/// Helper: reset keyboard state, feed scancodes, return collected events.
fn feed_and_collect(scancodes: &[u8]) -> alloc::vec::Vec<KeyEvent> {
    keyboard::reset();
    for &sc in scancodes {
        keyboard::handle_scancode(sc);
    }
    let mut events = alloc::vec::Vec::new();
    while let Some(ev) = keyboard::try_read_key() {
        events.push(ev);
    }
    events
}

/// Pressing 'a' (scancode 0x1E) should produce a Char('a') event.
pub fn test_key_a_press() -> TestResult {
    let events = feed_and_collect(&[0x1E]);
    if events.len() != 1 {
        return TestResult::Failed(format!("Expected 1 event, got {}", events.len()));
    }
    if events[0].event_type != KeyEventType::Char || events[0].character != b'a' {
        return TestResult::Failed(format!("Expected Char('a'), got {:?}", events[0]));
    }
    TestResult::Ok
}

/// Key release scancodes (bit 7 set) should NOT produce events.
pub fn test_key_release_ignored() -> TestResult {
    // 0x1E = 'a' press, 0x9E = 'a' release
    let events = feed_and_collect(&[0x1E, 0x9E]);
    if events.len() != 1 {
        return TestResult::Failed(format!(
            "Expected 1 event (press only), got {}",
            events.len()
        ));
    }
    TestResult::Ok
}

/// Shift + 'a' should produce Char('A').
pub fn test_shift_produces_uppercase() -> TestResult {
    // 0x2A = left shift press, 0x1E = 'a' press, 0xAA = left shift release
    let events = feed_and_collect(&[0x2A, 0x1E, 0xAA]);
    if events.len() != 1 {
        return TestResult::Failed(format!("Expected 1 event, got {}", events.len()));
    }
    if events[0].event_type != KeyEventType::Char || events[0].character != b'A' {
        return TestResult::Failed(format!("Expected Char('A'), got {:?}", events[0]));
    }
    TestResult::Ok
}

/// Enter key (scancode 0x1C) should produce an Enter event.
pub fn test_enter_key() -> TestResult {
    let events = feed_and_collect(&[0x1C]);
    if events.len() != 1 {
        return TestResult::Failed(format!("Expected 1 event, got {}", events.len()));
    }
    if events[0].event_type != KeyEventType::Enter {
        return TestResult::Failed(format!("Expected Enter, got {:?}", events[0]));
    }
    TestResult::Ok
}

/// Arrow up (extended: 0xE0, 0x48) should produce ArrowUp event.
pub fn test_arrow_keys() -> TestResult {
    let events = feed_and_collect(&[0xE0, 0x48]);
    if events.len() != 1 {
        return TestResult::Failed(format!("Expected 1 event, got {}", events.len()));
    }
    if events[0].event_type != KeyEventType::ArrowUp {
        return TestResult::Failed(format!("Expected ArrowUp, got {:?}", events[0]));
    }
    TestResult::Ok
}

/// Buffer should be empty after draining all events.
pub fn test_buffer_empty_after_drain() -> TestResult {
    keyboard::reset();
    keyboard::handle_scancode(0x1E); // 'a'
    let _ = keyboard::try_read_key(); // drain it
    if keyboard::has_key() {
        return TestResult::Failed("Buffer should be empty after draining".into());
    }
    if keyboard::try_read_key().is_some() {
        return TestResult::Failed("try_read_key should return None on empty buffer".into());
    }
    TestResult::Ok
}

/// Multiple keypresses should queue in order.
pub fn test_multiple_keys_order() -> TestResult {
    // 'a' = 0x1E, 'b' = 0x30, 'c' = 0x2E
    let events = feed_and_collect(&[0x1E, 0x30, 0x2E]);
    if events.len() != 3 {
        return TestResult::Failed(format!("Expected 3 events, got {}", events.len()));
    }
    if events[0].character != b'a' || events[1].character != b'b' || events[2].character != b'c' {
        return TestResult::Failed(format!(
            "Expected a,b,c but got {},{},{}",
            events[0].character as char,
            events[1].character as char,
            events[2].character as char
        ));
    }
    TestResult::Ok
}

/// Caps lock should toggle uppercase for letter keys.
pub fn test_capslock_toggle() -> TestResult {
    // 0x3A = capslock press, then 'a' press
    let events = feed_and_collect(&[0x3A, 0x1E]);
    if events.len() != 1 {
        return TestResult::Failed(format!("Expected 1 event, got {}", events.len()));
    }
    if events[0].character != b'A' {
        return TestResult::Failed(format!(
            "Expected 'A' with capslock on, got '{}'",
            events[0].character as char
        ));
    }

    // Second capslock press should toggle it off — but we need to keep state from above.
    // Reset and test the full sequence: capslock on, type 'a', capslock off, type 'a'
    keyboard::reset();
    keyboard::handle_scancode(0x3A); // capslock on
    keyboard::handle_scancode(0x1E); // 'a' -> 'A'
    keyboard::handle_scancode(0x3A); // capslock off
    keyboard::handle_scancode(0x1E); // 'a' -> 'a'

    let ev1 = keyboard::try_read_key().unwrap();
    let ev2 = keyboard::try_read_key().unwrap();
    if ev1.character != b'A' {
        return TestResult::Failed(format!(
            "First 'a' with capslock should be 'A', got '{}'",
            ev1.character as char
        ));
    }
    if ev2.character != b'a' {
        return TestResult::Failed(format!(
            "Second 'a' without capslock should be 'a', got '{}'",
            ev2.character as char
        ));
    }
    TestResult::Ok
}
