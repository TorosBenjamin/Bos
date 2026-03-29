//! Cross-platform TCP + DNS networking.
//!
//! Provides [`TcpStream`] (implementing `embedded_io::Read + Write`) and
//! [`resolve`] for DNS A-record lookups.
//!
//! On Linux these delegate to `std::net`. On Bos they use IPC channels to
//! communicate with `net_server`.

#[cfg(target_os = "linux")]
mod imp {
    use std::io::{Read as _, Write as _};
    use std::net::ToSocketAddrs;

    pub struct TcpStream {
        inner: std::net::TcpStream,
    }

    impl TcpStream {
        pub fn connect(ip: [u8; 4], port: u16) -> Option<Self> {
            let addr = std::net::SocketAddrV4::new(ip.into(), port);
            std::net::TcpStream::connect(addr).ok().map(|s| Self { inner: s })
        }
    }

    impl embedded_io::ErrorType for TcpStream {
        type Error = embedded_io::ErrorKind;
    }

    impl embedded_io::Read for TcpStream {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
            self.inner.read(buf).map_err(|_| embedded_io::ErrorKind::Other)
        }
    }

    impl embedded_io::Write for TcpStream {
        fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
            self.inner.write(buf).map_err(|_| embedded_io::ErrorKind::Other)
        }

        fn flush(&mut self) -> Result<(), Self::Error> {
            self.inner.flush().map_err(|_| embedded_io::ErrorKind::Other)
        }
    }

    /// DNS A-record lookup. Returns the first IPv4 address, or `None`.
    pub fn resolve(hostname: &[u8]) -> Option<[u8; 4]> {
        let host = core::str::from_utf8(hostname).ok()?;
        let addr_str = format!("{host}:0");
        let mut addrs = addr_str.to_socket_addrs().ok()?;
        addrs.find_map(|a| match a {
            std::net::SocketAddr::V4(v4) => Some(v4.ip().octets()),
            _ => None,
        })
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use ulib::net;

    /// Lazily-initialized net_server handle fd.
    static mut NET_FD: u32 = 0;

    fn net_fd() -> u32 {
        unsafe {
            if NET_FD == 0 {
                NET_FD = net::net_lookup();
            }
            NET_FD
        }
    }

    pub struct TcpStream {
        sock_id: u32,
        rx_fd: u32,
        /// Internal read buffer holding the last IPC message.
        rxbuf: [u8; 4096],
        /// Start offset of unconsumed data in `rxbuf`.
        rx_pos: usize,
        /// End offset (exclusive) of valid data in `rxbuf`.
        rx_len: usize,
        eof: bool,
    }

    impl TcpStream {
        pub fn connect(ip: [u8; 4], port: u16) -> Option<Self> {
            let fd = net_fd();
            let sock_id = net::net_connect(fd, ip, port)?;
            let rx_fd = net::net_recv_subscribe(fd, sock_id);
            Some(Self {
                sock_id,
                rx_fd,
                rxbuf: [0u8; 4096],
                rx_pos: 0,
                rx_len: 0,
                eof: false,
            })
        }
    }

    impl Drop for TcpStream {
        fn drop(&mut self) {
            let fd = net_fd();
            ulib::handle::close(self.rx_fd);
            net::net_close(fd, self.sock_id);
        }
    }

    impl embedded_io::ErrorType for TcpStream {
        type Error = embedded_io::ErrorKind;
    }

    impl embedded_io::Read for TcpStream {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
            // Return buffered data first.
            if self.rx_pos < self.rx_len {
                let avail = self.rx_len - self.rx_pos;
                let n = avail.min(buf.len());
                buf[..n].copy_from_slice(&self.rxbuf[self.rx_pos..self.rx_pos + n]);
                self.rx_pos += n;
                return Ok(n);
            }

            if self.eof {
                return Ok(0);
            }

            // Buffer empty — blocking read from the rx handle.
            match ulib::handle::read(self.rx_fd, &mut self.rxbuf) {
                Some(0) | None => {
                    self.eof = true;
                    Ok(0)
                }
                Some(n) => {
                    // Copy as much as the caller wants.
                    let copy = n.min(buf.len());
                    buf[..copy].copy_from_slice(&self.rxbuf[..copy]);
                    // Buffer the rest for next read.
                    self.rx_pos = copy;
                    self.rx_len = n;
                    Ok(copy)
                }
            }
        }
    }

    impl embedded_io::Write for TcpStream {
        fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
            net::net_send(net_fd(), self.sock_id, buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    /// Blocking DNS A-record resolve.
    ///
    /// Returns `Some([a, b, c, d])` on success, `None` on timeout or error.
    pub fn resolve(hostname: &[u8]) -> Option<[u8; 4]> {
        net::net_resolve(net_fd(), hostname)
    }
}

// ── Re-exports ───────────────────────────────────────────────────────────────

pub use imp::resolve;
pub use imp::TcpStream;
