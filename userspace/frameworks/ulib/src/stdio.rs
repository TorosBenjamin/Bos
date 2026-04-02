//! Standard I/O helpers for Bos userspace programs.
//!
//! Programs receive fd 0/1/2 (stdin/stdout/stderr) from the shell when spawned
//! via `run`. Reading from [`STDIN_FD`] blocks until a line is submitted by the
//! user; writing to [`STDOUT_FD`] sends text back to the shell's terminal.

/// File descriptor for standard input (readable pipe from the shell).
pub const STDIN_FD:  u32 = 0;
/// File descriptor for standard output (writable pipe to the shell).
pub const STDOUT_FD: u32 = 1;
/// File descriptor for standard error (writable pipe to the shell).
pub const STDERR_FD: u32 = 2;

/// Blocking read from stdin into `buf`. Returns bytes read (0 = EOF).
pub fn read(buf: &mut [u8]) -> usize {
    crate::handle::read(STDIN_FD, buf).unwrap_or(0)
}

/// Read one line from stdin (blocking). Stops at `\n` or EOF.
/// Strips the trailing `\n`; does NOT NUL-terminate.
/// Returns number of bytes placed in `buf`.
pub fn read_line(buf: &mut [u8]) -> usize {
    let mut total = 0;
    while total < buf.len() {
        let mut byte = [0u8; 1];
        match crate::handle::read(STDIN_FD, &mut byte) {
            Some(0) | None => break,
            Some(_) => {
                if byte[0] == b'\n' { break; }
                buf[total] = byte[0];
                total += 1;
            }
        }
    }
    total
}

/// Blocking write to stdout.
pub fn write(buf: &[u8]) {
    let _ = crate::handle::write(STDOUT_FD, buf);
}

/// Write to stderr.
pub fn write_err(buf: &[u8]) {
    let _ = crate::handle::write(STDERR_FD, buf);
}

/// Write a byte slice followed by a newline to stdout.
pub fn writeln(buf: &[u8]) {
    write(buf);
    write(b"\n");
}
