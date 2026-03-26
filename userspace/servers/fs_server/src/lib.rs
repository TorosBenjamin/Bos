//! # fs_server
//!
//! Custom FAT32 filesystem implementation for Bos OS.
//!
//! The [`fat32`] module contains a `#![no_std]` FAT32 driver built around the
//! [`fat32::BlockDev`] trait, which abstracts over the physical disk. In
//! production, `IpcDisk` sends block read/write requests to the IDE driver via
//! IPC. In tests, `MemDisk` provides an in-memory disk backed by a `Vec<u8>`.
//!
//! ## Capabilities
//!
//! - Mount from raw FAT32 partition (no MBR)
//! - Case-insensitive 8.3 filename lookup (no LFN support)
//! - Hierarchical directory traversal (`lookup("SUBDIR/FILE.TXT")`)
//! - Full file read via cluster chain traversal with single-sector FAT cache
//! - Root-level file write (create or overwrite)
//! - 28 unit tests validated against the `fatfs` crate
//!
//! ## Testing
//!
//! ```sh
//! cargo test -p fs_server
//! ```

#![cfg_attr(not(test), no_std)]

pub mod fat32;
