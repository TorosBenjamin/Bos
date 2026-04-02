# Filesystem Server

Before modifying this component, read `docs/fs_server.md` for the full architecture (FAT32 driver, IPC protocol, shared buffer pattern, IDE communication).

## Quick Reference

- **Tests**: `cargo test -p fs_server` (28 tests)
- **IPC protocol**: `[type: u8][request struct][reply_ep: u64 LE]` — use `ptr::read_unaligned` for struct reads
- **BlockDev trait**: `IpcDisk` (production, IPC to IDE driver) and `MemDisk` (tests, in-memory Vec<u8>)
- **FAT32 limitations**: 8.3 filenames only (no LFN), write_file only supports root directory, no MBR (raw FAT32 partition)
- **Disk image**: 64 MB raw FAT32, created by build.rs via `fatfs` crate. Must be >= 65,525 data clusters for FAT32 (smaller = FAT16).

## Key Files

- `src/fat32.rs` — FAT32 driver (BlockDev trait, Fat32 struct, all filesystem operations)
- `src/server.rs` — IPC dispatch, IpcDisk, request handlers
- `src/main.rs` — service registration, entry point
- `src/lib.rs` — test-only wrapper exposing fat32 module
