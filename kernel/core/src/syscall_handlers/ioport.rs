/// Syscall: read from an x86 I/O port.
///
/// args: port (u16), width (1=byte, 2=word, 4=dword)
/// returns: read value (zero-extended), or u64::MAX on invalid width
pub fn sys_ioport_read(port: u64, width: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    if port > 0xFFFF {
        return u64::MAX;
    }
    let port = port as u16;
    unsafe {
        match width {
            1 => x86::io::inb(port) as u64,
            2 => x86::io::inw(port) as u64,
            4 => x86::io::inl(port) as u64,
            _ => u64::MAX,
        }
    }
}

/// Syscall: write to an x86 I/O port.
///
/// args: port (u16), value, width (1=byte, 2=word, 4=dword)
/// returns: 0 on success, u64::MAX on invalid width
pub fn sys_ioport_write(port: u64, value: u64, width: u64, _: u64, _: u64, _: u64) -> u64 {
    if port > 0xFFFF {
        return u64::MAX;
    }
    let port = port as u16;
    unsafe {
        match width {
            1 => x86::io::outb(port, value as u8),
            2 => x86::io::outw(port, value as u16),
            4 => x86::io::outl(port, value as u32),
            _ => return u64::MAX,
        }
    }
    0
}
