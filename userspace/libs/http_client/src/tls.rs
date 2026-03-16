//! TLS transport (AES-128-GCM and AES-256-GCM cipher suites).

use alloc::vec::Vec;
use bos_std::net;
use crate::{HttpError, MAX_RESPONSE_BYTES};

// ── Hardware RNG via RDRAND ─────────────────────────────────────────────────

pub(crate) struct RdRandRng;

impl rand_core::CryptoRng for RdRandRng {}

impl rand_core::RngCore for RdRandRng {
    fn next_u32(&mut self) -> u32 {
        self.next_u64() as u32
    }

    fn next_u64(&mut self) -> u64 {
        let val: u64;
        unsafe {
            core::arch::asm!(
                "2: rdrand {val}",
                "jnc 2b",
                val = out(reg) val,
            );
        }
        val
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        let mut i = 0;
        while i + 8 <= dest.len() {
            let val = self.next_u64();
            dest[i..i + 8].copy_from_slice(&val.to_le_bytes());
            i += 8;
        }
        if i < dest.len() {
            let val = self.next_u64();
            let bytes = val.to_le_bytes();
            let remaining = dest.len() - i;
            dest[i..i + remaining].copy_from_slice(&bytes[..remaining]);
        }
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

// ── TLS request with AES-128-GCM ────────────────────────────────────────────

pub(crate) fn do_tls_request_128(
    stream: &mut net::TcpStream,
    server_name: &str,
    request: &[u8],
    raw: &mut Vec<u8>,
) -> Result<(), HttpError> {
    // Some servers send TLS records slightly over 16 KiB; use 17 KiB buffers.
    let mut read_buf = alloc::vec![0u8; 17408];
    let mut write_buf = alloc::vec![0u8; 17408];

    let config = embedded_tls::TlsConfig::new()
        .with_server_name(server_name);

    let mut tls: embedded_tls::blocking::TlsConnection<_, embedded_tls::Aes128GcmSha256> =
        embedded_tls::blocking::TlsConnection::new(stream, &mut read_buf, &mut write_buf);

    let context = embedded_tls::TlsContext::new(
        &config,
        embedded_tls::UnsecureProvider::new::<embedded_tls::Aes128GcmSha256>(RdRandRng),
    );
    tls.open(context).map_err(|_| HttpError::TlsError)?;

    write_all_tls_128(&mut tls, request)?;
    tls.flush().map_err(|_| HttpError::TlsError)?;

    read_response_tls_128(&mut tls, raw)?;
    let _ = tls.close();
    Ok(())
}

// ── TLS request with AES-256-GCM ────────────────────────────────────────────

pub(crate) fn do_tls_request_256(
    stream: &mut net::TcpStream,
    server_name: &str,
    request: &[u8],
    raw: &mut Vec<u8>,
) -> Result<(), HttpError> {
    let mut read_buf = alloc::vec![0u8; 17408];
    let mut write_buf = alloc::vec![0u8; 17408];

    let config = embedded_tls::TlsConfig::new()
        .with_server_name(server_name);

    let mut tls: embedded_tls::blocking::TlsConnection<_, embedded_tls::Aes256GcmSha384> =
        embedded_tls::blocking::TlsConnection::new(stream, &mut read_buf, &mut write_buf);

    let context = embedded_tls::TlsContext::new(
        &config,
        embedded_tls::UnsecureProvider::new::<embedded_tls::Aes256GcmSha384>(RdRandRng),
    );
    tls.open(context).map_err(|_| HttpError::TlsError)?;

    write_all_tls_256(&mut tls, request)?;
    tls.flush().map_err(|_| HttpError::TlsError)?;

    read_response_tls_256(&mut tls, raw)?;
    let _ = tls.close();
    Ok(())
}

// ── Write helpers ────────────────────────────────────────────────────────────

fn write_all_tls_128<S: embedded_io::Read + embedded_io::Write>(
    tls: &mut embedded_tls::blocking::TlsConnection<S, embedded_tls::Aes128GcmSha256>,
    mut data: &[u8],
) -> Result<(), HttpError> {
    while !data.is_empty() {
        let n = tls.write(data).map_err(|_| HttpError::TlsError)?;
        if n == 0 { return Err(HttpError::TlsError); }
        data = &data[n..];
    }
    Ok(())
}

fn write_all_tls_256<S: embedded_io::Read + embedded_io::Write>(
    tls: &mut embedded_tls::blocking::TlsConnection<S, embedded_tls::Aes256GcmSha384>,
    mut data: &[u8],
) -> Result<(), HttpError> {
    while !data.is_empty() {
        let n = tls.write(data).map_err(|_| HttpError::TlsError)?;
        if n == 0 { return Err(HttpError::TlsError); }
        data = &data[n..];
    }
    Ok(())
}

// ── Read helpers ─────────────────────────────────────────────────────────────

fn read_response_tls_128<S: embedded_io::Read + embedded_io::Write>(
    tls: &mut embedded_tls::blocking::TlsConnection<S, embedded_tls::Aes128GcmSha256>,
    raw: &mut Vec<u8>,
) -> Result<(), HttpError> {
    let mut chunk = [0u8; 4096];
    loop {
        match tls.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                if raw.len() + n > MAX_RESPONSE_BYTES { return Err(HttpError::TooLarge); }
                raw.extend_from_slice(&chunk[..n]);
            }
            Err(_) => break,
        }
    }
    Ok(())
}

fn read_response_tls_256<S: embedded_io::Read + embedded_io::Write>(
    tls: &mut embedded_tls::blocking::TlsConnection<S, embedded_tls::Aes256GcmSha384>,
    raw: &mut Vec<u8>,
) -> Result<(), HttpError> {
    let mut chunk = [0u8; 4096];
    loop {
        match tls.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                if raw.len() + n > MAX_RESPONSE_BYTES { return Err(HttpError::TooLarge); }
                raw.extend_from_slice(&chunk[..n]);
            }
            Err(_) => break,
        }
    }
    Ok(())
}
