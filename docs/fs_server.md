# Filesystem Server Architecture

The filesystem server provides FAT32 filesystem access to userspace programs.
It reads/writes an IDE disk via IPC to the IDE driver, and serves file
operations to clients through the kernel's service registry.

**Source:** `userspace/servers/fs_server/`

## Module Layout

```
src/
  main.rs      Entry point — registers "fatfs" service, enters server loop
  server.rs    IPC message dispatch, IDE client (IpcDisk), request handlers
  fat32.rs     Custom FAT32 driver (777 lines) + 28 unit tests
  lib.rs       Test-only wrapper (exposes fat32 for `cargo test -p fs_server`)
```

## Data Flow

```
  Client (e.g. ELF loader)
    |  IPC request (channel)
    v
  fs_server
    |  Fat32<IpcDisk>
    |    |  IPC to IDE driver (block read/write)
    |    v
    |  IDE server (userspace)
    |    |  syscall (BlockReadSectors / BlockWriteSectors)
    |    v
    |  Kernel IDE driver (PIO 0x1F0-0x1F7)
    |    |
    v    v
  QEMU HDD (disk.img, raw FAT32, no MBR)
```

## Startup

1. Create IPC channel (capacity 16).
2. Register as `"fatfs"` service.
3. Wait for the `"ide"` service to appear (poll with `sys_lookup_service`).
4. Mount FAT32 volume: read BPB from sector 0, validate signature and FAT type.
5. Enter blocking server loop: `sys_channel_recv` → dispatch → `sys_channel_send`.

## FAT32 Driver (`fat32.rs`)

### BlockDev Trait

```rust
pub trait BlockDev {
    fn read(&mut self, lba: u64, buf: &mut [u8; 512]) -> bool;
    fn write(&mut self, lba: u64, buf: &[u8; 512]) -> bool;
    fn read_sectors(&mut self, lba: u64, count: u32, buf: &mut [u8]) -> bool;
}
```

Two implementations:
- **IpcDisk** (production) — sends block I/O requests to the IDE server over IPC.
- **MemDisk** (tests) — in-memory `Vec<u8>` formatted via the `fatfs` crate.

### Key Types

- `Fat32<D: BlockDev>` — main filesystem handle. Holds BPB-derived layout
  constants and a single-sector FAT cache.
- `Entry` — directory entry: cluster, size, is_dir, 8.3 name.

### Operations

| Method | Description |
|--------|-------------|
| `mount(disk)` | Read BPB, validate FAT32 (rejects FAT12/16), return `Fat32` |
| `lookup(path)` | Traverse directories, case-insensitive 8.3 match |
| `read_dir(cluster, out)` | List directory entries (skips LFN/volume-ID) |
| `read_file(cluster, size, buf)` | Follow cluster chain, read into buffer |
| `write_file(filename, data)` | Create/overwrite file in root directory only |

### FAT Cache

The driver caches the most recently read FAT sector (512 bytes). Since
consecutive clusters in a file usually share the same FAT sector, this avoids
redundant disk reads during cluster chain traversal.

### Limitations

- 8.3 filenames only (Long File Name entries are skipped).
- `write_file` only supports root-level files (no subdirectory creation).
- No MBR support — expects a raw FAT32 volume starting at sector 0.

## IPC Protocol

### Message Format

```
[type: u8][request struct][reply_ep: u64 LE]
```

The server reads from its service channel, dispatches by message type, sends
the response on the client's reply endpoint, then closes the reply endpoint.

### Request Types

| Type | Request | Response |
|------|---------|----------|
| MapFile (0) | path → | shared_buf_id, file_size |
| StatFile (1) | path → | size, is_dir |
| ReadDir (2) | path → | count, entries[48] |
| WriteFile (3) | path, shared_buf_id, size → | result |

### Shared Buffer Pattern (MapFile)

1. Client sends `MapFileRequest` with a file path.
2. Server looks up the file, allocates a shared physical buffer (`sys_create_shared_buf`).
3. Server reads the entire file directly into the shared buffer (zero-copy to the server).
4. Server sends back the `shared_buf_id` and file size.
5. Client maps the buffer into its own address space (`sys_map_shared_buf`).
6. Client reads the data and destroys the buffer when done.

### Unaligned Access

Request structs sit at offset 1 in the message buffer (after the type byte),
which is unaligned for structs with 8-byte fields. All handlers use
`ptr::read_unaligned()` to avoid UB on x86-64.

## Testing

```sh
cargo test -p fs_server   # 28 unit tests
```

Tests use `MemDisk` (40 MB FAT32 image created via `fatfs` crate) and
cross-validate reads/writes against `fatfs` to ensure format compatibility.

Test categories: name encoding (4), name matching (4), mount validation (4),
lookup (7), read_dir (3), read_file (3), write_file (3).

## Disk Image

The build system creates a 64 MB raw FAT32 image (`disk.img`) using the `fatfs`
crate at build time. QEMU mounts it as `-drive file=disk.img,if=ide,format=raw`.
The 64 MB size ensures >= 65,525 data clusters, which is the threshold for FAT32
(smaller disks may be formatted as FAT16).
