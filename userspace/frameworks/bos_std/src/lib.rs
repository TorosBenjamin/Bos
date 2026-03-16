//! Platform-agnostic standard library for Bos applications.
//!
//! On Linux, delegates to `std`. On Bos, delegates to `ulib` / kernel IPC.
//! This allows userspace apps and libraries (e.g. `http_client`) to be
//! compiled and tested on the host without modification.

#![cfg_attr(not(target_os = "linux"), no_std)]

pub mod net;
