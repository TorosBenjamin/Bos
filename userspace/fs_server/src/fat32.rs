/// Minimal read/write FAT32 driver.
///
/// Assumes the FAT32 volume starts at LBA 0 (raw image, no MBR).
/// Short filename (8.3) only — sufficient for app binaries.
///
/// All disk I/O is abstracted behind `BlockDev`, making the module
/// unit-testable with an in-memory mock.

// ─── Block device abstraction ──────────────────────────────────────────────────

pub trait BlockDev {
    fn read(&mut self, lba: u64, buf: &mut [u8; 512]) -> bool;
    fn write(&mut self, lba: u64, buf: &[u8; 512]) -> bool;
}

// ─── Constants ─────────────────────────────────────────────────────────────────

const FAT32_EOC:      u32 = 0x0FFF_FFF8;
const ATTR_DIRECTORY: u8  = 0x10;
const ATTR_LFN:       u8  = 0x0F;
const ATTR_VOLUME_ID: u8  = 0x08;

// ─── BPB (BIOS Parameter Block) ───────────────────────────────────────────────

#[repr(C, packed)]
struct Bpb {
    jump:              [u8; 3],
    oem:               [u8; 8],
    bytes_per_sector:  u16,
    sectors_per_clus:  u8,
    reserved_sectors:  u16,
    num_fats:          u8,
    root_entry_count:  u16,
    total_sectors_16:  u16,
    media:             u8,
    fat_size_16:       u16,
    sectors_per_track: u16,
    num_heads:         u16,
    hidden_sectors:    u32,
    total_sectors_32:  u32,
    // FAT32 extension
    fat_size_32:       u32,
    ext_flags:         u16,
    fs_version:        u16,
    root_cluster:      u32,
    fs_info:           u16,
    backup_boot_sector: u16,
    _reserved:         [u8; 12],
    drive_number:      u8,
    _reserved2:        u8,
    boot_signature:    u8,
    volume_id:         u32,
    volume_label:      [u8; 11],
    fs_type:           [u8; 8],
}

// ─── Directory entry (32 bytes) ────────────────────────────────────────────────

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct RawDirEntry {
    name:        [u8; 8],
    ext:         [u8; 3],
    attr:        u8,
    _nt:         u8,
    _crt_tenths: u8,
    _crt_time:   u16,
    _crt_date:   u16,
    _acc_date:   u16,
    cluster_hi:  u16,
    _mod_time:   u16,
    _mod_date:   u16,
    cluster_lo:  u16,
    size:        u32,
}

impl RawDirEntry {
    fn is_free(&self) -> bool { self.name[0] == 0x00 || self.name[0] == 0xE5 }
    fn is_end(&self) -> bool  { self.name[0] == 0x00 }
    fn is_lfn(&self) -> bool  { self.attr & ATTR_LFN == ATTR_LFN }
    fn is_volume_id(&self) -> bool { self.attr & ATTR_VOLUME_ID != 0 && !self.is_lfn() }
    fn is_dir(&self) -> bool  { self.attr & ATTR_DIRECTORY != 0 }

    fn cluster(&self) -> u32 {
        ((u16::from_le(self.cluster_hi) as u32) << 16)
            | u16::from_le(self.cluster_lo) as u32
    }
    fn size(&self) -> u32 { u32::from_le(self.size) }

    /// Returns the short 8.3 name as (buf, len), e.g. `"HELLO   TXT"` → `"HELLO.TXT"`.
    fn short_name(&self) -> ([u8; 12], usize) {
        let mut buf = [0u8; 12];
        let mut len = 0usize;
        let name_end = self.name.iter().rposition(|&b| b != b' ').map_or(0, |i| i + 1);
        buf[..name_end].copy_from_slice(&self.name[..name_end]);
        len += name_end;
        let ext_end = self.ext.iter().rposition(|&b| b != b' ').map_or(0, |i| i + 1);
        if ext_end > 0 {
            buf[len] = b'.';
            len += 1;
            buf[len..len + ext_end].copy_from_slice(&self.ext[..ext_end]);
            len += ext_end;
        }
        (buf, len)
    }
}

// ─── Public types ──────────────────────────────────────────────────────────────

pub struct Fat32<D> {
    pub disk:              D,
    pub reserved_sectors:  u32,
    pub fat_size:          u32,
    pub sectors_per_clus:  u32,
    pub root_cluster:      u32,
    pub first_data_sector: u32,
    pub bytes_per_sector:  u32,
}

/// A single file or directory entry returned by `lookup` / `read_dir`.
pub struct Entry {
    pub cluster:  u32,
    pub size:     u32,
    pub is_dir:   bool,
    pub name:     [u8; 12],
    pub name_len: usize,
}

// ─── Implementation ────────────────────────────────────────────────────────────

impl<D: BlockDev> Fat32<D> {
    /// Mount a FAT32 volume: parse BPB from sector 0 and compute layout constants.
    pub fn mount(mut disk: D) -> Option<Self> {
        let mut sec = [0u8; 512];
        if !disk.read(0, &mut sec) { return None; }
        if sec[510] != 0x55 || sec[511] != 0xAA { return None; }

        let bpb = unsafe { &*(sec.as_ptr() as *const Bpb) };

        let bytes_per_sector  = u16::from_le(bpb.bytes_per_sector) as u32;
        let sectors_per_clus  = bpb.sectors_per_clus as u32;
        let reserved_sectors  = u16::from_le(bpb.reserved_sectors) as u32;
        let num_fats          = bpb.num_fats as u32;
        let fat_size_16       = u16::from_le(bpb.fat_size_16) as u32;
        let fat_size_32       = u32::from_le(bpb.fat_size_32);

        // FAT12/FAT16 have fat_size_16 != 0; we only support FAT32.
        if fat_size_16 != 0 { return None; }
        let fat_size          = fat_size_32;
        let root_cluster      = u32::from_le(bpb.root_cluster);

        if fat_size == 0 || sectors_per_clus == 0 || bytes_per_sector == 0 { return None; }

        let first_data_sector = reserved_sectors + num_fats * fat_size;

        Some(Self { disk, reserved_sectors, fat_size, sectors_per_clus,
                    root_cluster, first_data_sector, bytes_per_sector })
    }

    fn cluster_to_lba(&self, cluster: u32) -> u64 {
        self.first_data_sector as u64 + (cluster as u64 - 2) * self.sectors_per_clus as u64
    }

    fn fat_entry(&mut self, cluster: u32) -> Option<u32> {
        let fat_offset = cluster as u64 * 4;
        let fat_sector = self.reserved_sectors as u64 + fat_offset / self.bytes_per_sector as u64;
        let off        = (fat_offset % self.bytes_per_sector as u64) as usize;
        let mut sec = [0u8; 512];
        if !self.disk.read(fat_sector, &mut sec) { return None; }
        Some(u32::from_le_bytes([sec[off], sec[off+1], sec[off+2], sec[off+3]]) & 0x0FFF_FFFF)
    }

    fn is_eoc(cluster: u32) -> bool { cluster >= FAT32_EOC }

    // ─── Public API ────────────────────────────────────────────────────────────

    /// Locate a file/directory by path relative to the root.
    /// Empty or `/` returns a synthetic root entry.
    pub fn lookup(&mut self, path: &str) -> Option<Entry> {
        let path = path.trim_matches('/');
        if path.is_empty() {
            return Some(Entry {
                cluster: self.root_cluster, size: 0, is_dir: true,
                name: { let mut b = [0u8; 12]; b[0] = b'/'; b }, name_len: 1,
            });
        }
        let mut cur = self.root_cluster;
        let mut remaining = path;
        loop {
            let (component, rest) = match remaining.find('/') {
                Some(i) => (&remaining[..i], remaining[i+1..].trim_matches('/')),
                None    => (remaining, ""),
            };
            let entry = self.find_in_dir(cur, component)?;
            if rest.is_empty() { return Some(entry); }
            if !entry.is_dir  { return None; }
            cur = entry.cluster;
            remaining = rest;
        }
    }

    /// List up to `out.len()` entries of the directory at `start_cluster`.
    /// Returns the number of entries written.
    pub fn read_dir(&mut self, start_cluster: u32, out: &mut [Entry]) -> usize {
        let epe = (self.bytes_per_sector / 32) as usize; // entries per sector
        let mut count = 0;
        let mut cluster = start_cluster;

        'outer: while !Self::is_eoc(cluster) && cluster >= 2 {
            let lba = self.cluster_to_lba(cluster);
            for s in 0..self.sectors_per_clus {
                let mut sec = [0u8; 512];
                if !self.disk.read(lba + s as u64, &mut sec) { break 'outer; }
                let raw = unsafe { core::slice::from_raw_parts(sec.as_ptr() as *const RawDirEntry, epe) };
                for de in raw {
                    if de.is_end() { break 'outer; }
                    if de.is_free() || de.is_lfn() || de.is_volume_id() { continue; }
                    if count >= out.len() { break 'outer; }
                    let (name, name_len) = de.short_name();
                    out[count] = Entry { cluster: de.cluster(), size: de.size(),
                                         is_dir: de.is_dir(), name, name_len };
                    count += 1;
                }
            }
            cluster = self.fat_entry(cluster).unwrap_or(FAT32_EOC);
        }
        count
    }

    /// Read a file's data into `buf`. Returns bytes read.
    pub fn read_file(&mut self, start_cluster: u32, size: u32, buf: *mut u8) -> usize {
        let mut cluster = start_cluster;
        let mut written = 0usize;
        let total = size as usize;

        while !Self::is_eoc(cluster) && cluster >= 2 && written < total {
            let lba = self.cluster_to_lba(cluster);
            for s in 0..self.sectors_per_clus {
                if written >= total { break; }
                let mut sec = [0u8; 512];
                if !self.disk.read(lba + s as u64, &mut sec) { return written; }
                let to_copy = (total - written).min(512);
                unsafe { core::ptr::copy_nonoverlapping(sec.as_ptr(), buf.add(written), to_copy); }
                written += to_copy;
            }
            cluster = self.fat_entry(cluster).unwrap_or(FAT32_EOC);
        }
        written
    }

    /// Create or overwrite a file in the root directory.
    /// Only root-level files are supported (no subdirectory writes).
    pub fn write_file(&mut self, filename: &str, data: &[u8]) -> bool {
        let (name8, ext3) = split_83(filename);
        let lba_base = self.cluster_to_lba(self.root_cluster);
        let epe = (self.bytes_per_sector / 32) as usize;

        let mut entry_lba = 0u64;
        let mut entry_idx = 0usize;
        let mut found = false;

        'search: for s in 0..self.sectors_per_clus {
            let mut sec = [0u8; 512];
            if !self.disk.read(lba_base + s as u64, &mut sec) { return false; }
            for i in 0..epe {
                let off = i * 32;
                let de = unsafe { &*(sec.as_ptr().add(off) as *const RawDirEntry) };
                if de.is_end() {
                    if !found { entry_lba = lba_base + s as u64; entry_idx = i; found = true; }
                    break 'search;
                }
                if de.is_free() || de.is_lfn() || de.is_volume_id() { continue; }
                let (n, e) = (&de.name, &de.ext);
                if n == &name8 && e == &ext3 {
                    // Free existing cluster chain
                    let mut fc = de.cluster();
                    while !Self::is_eoc(fc) && fc >= 2 {
                        let next = self.fat_entry(fc).unwrap_or(FAT32_EOC);
                        self.set_fat_entry(fc, 0);
                        fc = next;
                    }
                    entry_lba = lba_base + s as u64;
                    entry_idx = i;
                    found = true;
                    break 'search;
                }
            }
        }
        if !found { return false; }

        // Allocate cluster chain and write data
        let first_cluster = if data.is_empty() { 0 } else {
            match self.alloc_cluster() { Some(c) => c, None => return false }
        };

        let mut cur = first_cluster;
        let mut rem = data;
        while !rem.is_empty() {
            let clus_lba = self.cluster_to_lba(cur);
            for s in 0..self.sectors_per_clus {
                let mut sec = [0u8; 512];
                let to_write = rem.len().min(512);
                sec[..to_write].copy_from_slice(&rem[..to_write]);
                if !self.disk.write(clus_lba + s as u64, &sec) { return false; }
                rem = &rem[to_write..];
                if rem.is_empty() { break; }
            }
            if rem.is_empty() { break; }
            let next = match self.alloc_cluster() { Some(c) => c, None => return false };
            if !self.set_fat_entry(cur, next) { return false; }
            cur = next;
        }

        // Write directory entry
        let mut sec = [0u8; 512];
        if !self.disk.read(entry_lba, &mut sec) { return false; }
        let de = unsafe { &mut *(sec.as_mut_ptr().add(entry_idx * 32) as *mut RawDirEntry) };
        *de = RawDirEntry {
            name: name8, ext: ext3, attr: 0x20,
            _nt: 0, _crt_tenths: 0, _crt_time: 0, _crt_date: 0, _acc_date: 0,
            cluster_hi: ((first_cluster >> 16) as u16).to_le(),
            _mod_time: 0, _mod_date: 0,
            cluster_lo: (first_cluster as u16).to_le(),
            size: (data.len() as u32).to_le(),
        };
        self.disk.write(entry_lba, &sec)
    }

    // ─── Private helpers ───────────────────────────────────────────────────────

    fn find_in_dir(&mut self, start_cluster: u32, name: &str) -> Option<Entry> {
        let epe = (self.bytes_per_sector / 32) as usize;
        let mut cluster = start_cluster;
        while !Self::is_eoc(cluster) && cluster >= 2 {
            let lba = self.cluster_to_lba(cluster);
            for s in 0..self.sectors_per_clus {
                let mut sec = [0u8; 512];
                if !self.disk.read(lba + s as u64, &mut sec) { return None; }
                let raw = unsafe { core::slice::from_raw_parts(sec.as_ptr() as *const RawDirEntry, epe) };
                for de in raw {
                    if de.is_end() { return None; }
                    if de.is_free() || de.is_lfn() || de.is_volume_id() { continue; }
                    let (sname, slen) = de.short_name();
                    let s = core::str::from_utf8(&sname[..slen]).unwrap_or("");
                    if names_match(s, name) {
                        return Some(Entry { cluster: de.cluster(), size: de.size(),
                                            is_dir: de.is_dir(), name: sname, name_len: slen });
                    }
                }
            }
            cluster = self.fat_entry(cluster).unwrap_or(FAT32_EOC);
        }
        None
    }

    fn alloc_cluster(&mut self) -> Option<u32> {
        let fat_lba = self.reserved_sectors as u64;
        for fat_sec in 0..self.fat_size as u64 {
            let mut sec = [0u8; 512];
            if !self.disk.read(fat_lba + fat_sec, &mut sec) { return None; }
            for i in 0..(512 / 4) {
                let cluster = (fat_sec * (512 / 4) as u64 + i as u64) as u32;
                if cluster < 2 { continue; }
                let off = i * 4;
                let entry = u32::from_le_bytes([sec[off], sec[off+1], sec[off+2], sec[off+3]]) & 0x0FFF_FFFF;
                if entry == 0 {
                    let eoc = 0x0FFF_FFFF_u32.to_le_bytes();
                    sec[off..off+4].copy_from_slice(&eoc);
                    if !self.disk.write(fat_lba + fat_sec, &sec) { return None; }
                    return Some(cluster);
                }
            }
        }
        None
    }

    fn set_fat_entry(&mut self, cluster: u32, next: u32) -> bool {
        let fat_offset = cluster as u64 * 4;
        let fat_lba = self.reserved_sectors as u64 + fat_offset / self.bytes_per_sector as u64;
        let off = (fat_offset % self.bytes_per_sector as u64) as usize;
        let mut sec = [0u8; 512];
        if !self.disk.read(fat_lba, &mut sec) { return false; }
        let val = (next & 0x0FFF_FFFF).to_le_bytes();
        sec[off..off+4].copy_from_slice(&val);
        self.disk.write(fat_lba, &sec)
    }
}

// ─── Helpers ───────────────────────────────────────────────────────────────────

pub fn names_match(short: &str, query: &str) -> bool {
    if short.len() != query.len() { return false; }
    short.chars().zip(query.chars()).all(|(a, b)| a.eq_ignore_ascii_case(&b))
}

pub fn split_83(filename: &str) -> ([u8; 8], [u8; 3]) {
    let mut name = [b' '; 8];
    let mut ext  = [b' '; 3];
    let (base, extension) = match filename.rfind('.') {
        Some(i) => (&filename[..i], &filename[i+1..]),
        None    => (filename, ""),
    };
    for (i, b) in base.bytes().take(8).enumerate()      { name[i] = b.to_ascii_uppercase(); }
    for (i, b) in extension.bytes().take(3).enumerate() { ext[i]  = b.to_ascii_uppercase(); }
    (name, ext)
}

// ─── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    extern crate std;
    use std::io::{Cursor, Write, Read};
    use std::vec::Vec;
    use super::*;

    // ── Mock block device backed by Vec<u8> ──────────────────────────────────

    struct MemDisk(Vec<u8>);

    impl BlockDev for MemDisk {
        fn read(&mut self, lba: u64, buf: &mut [u8; 512]) -> bool {
            let off = lba as usize * 512;
            if off + 512 > self.0.len() { return false; }
            buf.copy_from_slice(&self.0[off..off + 512]);
            true
        }
        fn write(&mut self, lba: u64, buf: &[u8; 512]) -> bool {
            let off = lba as usize * 512;
            if off + 512 > self.0.len() { return false; }
            self.0[off..off + 512].copy_from_slice(buf);
            true
        }
    }

    /// Create an in-memory FAT32 image using the `fatfs` std crate.
    ///
    /// The disk must be large enough that `determine_fs_geometry` selects FAT32.
    /// With the `Fat32` type hint, `fatfs` uses 512 bytes/cluster. FAT32 requires
    /// ≥65 525 data clusters, so the minimum disk size is ~34 MB. We use 40 MB.
    fn make_disk() -> MemDisk {
        const SIZE: usize = 40 * 1024 * 1024;
        let mut cursor = Cursor::new(vec![0u8; SIZE]);
        fatfs::format_volume(
            &mut cursor,
            fatfs::FormatVolumeOptions::new().fat_type(fatfs::FatType::Fat32),
        ).expect("format_volume failed");
        MemDisk(cursor.into_inner())
    }

    /// Write a file to the disk via `fatfs` (std) and return the disk.
    fn disk_with_file(name: &str, content: &[u8]) -> MemDisk {
        let mut disk = make_disk();
        {
            let mut cursor = Cursor::new(&mut disk.0);
            let fs = fatfs::FileSystem::new(&mut cursor, fatfs::FsOptions::new())
                .expect("FileSystem::new failed");
            let mut f = fs.root_dir().create_file(name).expect("create_file failed");
            f.truncate().unwrap();
            f.write_all(content).unwrap();
        }
        disk
    }

    /// Read a file from the disk via `fatfs` (std) and return its contents.
    fn read_via_fatfs(disk: &mut MemDisk, name: &str) -> Vec<u8> {
        let mut cursor = Cursor::new(&mut disk.0);
        let fs = fatfs::FileSystem::new(&mut cursor, fatfs::FsOptions::new()).unwrap();
        let mut f = fs.root_dir().open_file(name).unwrap();
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).unwrap();
        buf
    }

    // ── split_83 ─────────────────────────────────────────────────────────────

    #[test]
    fn split83_with_extension() {
        let (n, e) = split_83("hello.txt");
        assert_eq!(&n, b"HELLO   ");
        assert_eq!(&e, b"TXT");
    }

    #[test]
    fn split83_no_extension() {
        let (n, e) = split_83("makefile");
        assert_eq!(&n, b"MAKEFILE");
        assert_eq!(&e, b"   ");
    }

    #[test]
    fn split83_truncates_long_name() {
        let (n, e) = split_83("toolongname.rs");
        assert_eq!(&n, b"TOOLONGN");
        assert_eq!(&e, b"RS ");
    }

    #[test]
    fn split83_dot_only() {
        let (n, _) = split_83(".");
        assert_eq!(n[0], b' '); // base before "." is empty
    }

    // ── names_match ──────────────────────────────────────────────────────────

    #[test]
    fn names_match_exact() {
        assert!(names_match("HELLO.TXT", "HELLO.TXT"));
    }

    #[test]
    fn names_match_case_insensitive() {
        assert!(names_match("HELLO.TXT", "hello.txt"));
        assert!(names_match("CUBE1.ELF", "cube1.elf"));
    }

    #[test]
    fn names_match_different_lengths() {
        assert!(!names_match("ABC", "ABCD"));
    }

    #[test]
    fn names_match_different_content() {
        assert!(!names_match("FOO.TXT", "BAR.TXT"));
    }

    // ── mount ────────────────────────────────────────────────────────────────

    #[test]
    fn mount_valid_fat32() {
        let disk = make_disk();
        let fs = Fat32::mount(disk);
        assert!(fs.is_some(), "mount should succeed on a valid FAT32 image");
    }

    #[test]
    fn mount_empty_disk_fails() {
        let disk = MemDisk(vec![0u8; 4 * 1024 * 1024]);
        assert!(Fat32::mount(disk).is_none(), "mount on blank disk should fail");
    }

    #[test]
    fn mount_root_cluster_is_2() {
        let disk = make_disk();
        let fs = Fat32::mount(disk).unwrap();
        assert_eq!(fs.root_cluster, 2);
    }

    #[test]
    fn mount_sector_size_is_512() {
        let disk = make_disk();
        let fs = Fat32::mount(disk).unwrap();
        assert_eq!(fs.bytes_per_sector, 512);
    }

    // ── lookup ───────────────────────────────────────────────────────────────

    #[test]
    fn lookup_root() {
        let disk = make_disk();
        let mut fs = Fat32::mount(disk).unwrap();
        let entry = fs.lookup("/").unwrap();
        assert!(entry.is_dir);
    }

    #[test]
    fn lookup_empty_path_is_root() {
        let disk = make_disk();
        let mut fs = Fat32::mount(disk).unwrap();
        assert!(fs.lookup("").unwrap().is_dir);
    }

    #[test]
    fn lookup_existing_file() {
        let disk = disk_with_file("HELLO.TXT", b"world");
        let mut fs = Fat32::mount(disk).unwrap();
        let entry = fs.lookup("HELLO.TXT").unwrap();
        assert!(!entry.is_dir);
        assert_eq!(entry.size, 5);
    }

    #[test]
    fn lookup_case_insensitive() {
        let disk = disk_with_file("README.TXT", b"data");
        let mut fs = Fat32::mount(disk).unwrap();
        assert!(fs.lookup("readme.txt").is_some());
        assert!(fs.lookup("README.TXT").is_some());
        assert!(fs.lookup("Readme.Txt").is_some());
    }

    #[test]
    fn lookup_missing_file() {
        let disk = make_disk();
        let mut fs = Fat32::mount(disk).unwrap();
        assert!(fs.lookup("NOSUCH.TXT").is_none());
    }

    #[test]
    fn lookup_file_not_dir() {
        let disk = disk_with_file("HELLO.TXT", b"x");
        let mut fs = Fat32::mount(disk).unwrap();
        let e = fs.lookup("HELLO.TXT").unwrap();
        assert!(!e.is_dir);
    }

    // ── read_dir ─────────────────────────────────────────────────────────────

    #[test]
    fn read_dir_empty() {
        let disk = make_disk();
        let mut fs = Fat32::mount(disk).unwrap();
        let mut out: [Entry; 4] = core::array::from_fn(|_| Entry { cluster:0, size:0, is_dir:false, name:[0;12], name_len:0 });
        let count = fs.read_dir(fs.root_cluster, &mut out);
        assert_eq!(count, 0);
    }

    #[test]
    fn read_dir_one_file() {
        let disk = disk_with_file("DATA.BIN", b"hello");
        let mut fs = Fat32::mount(disk).unwrap();
        let mut out: [Entry; 4] = core::array::from_fn(|_| Entry { cluster:0, size:0, is_dir:false, name:[0;12], name_len:0 });
        let count = fs.read_dir(fs.root_cluster, &mut out);
        assert_eq!(count, 1);
        let name = core::str::from_utf8(&out[0].name[..out[0].name_len]).unwrap();
        assert!(names_match(name, "DATA.BIN"));
        assert_eq!(out[0].size, 5);
    }

    #[test]
    fn read_dir_multiple_files() {
        let mut disk = make_disk();
        {
            let mut cursor = Cursor::new(&mut disk.0);
            let fs = fatfs::FileSystem::new(&mut cursor, fatfs::FsOptions::new()).unwrap();
            for name in &["FILE1.TXT", "FILE2.TXT", "FILE3.TXT"] {
                let mut f = fs.root_dir().create_file(name).unwrap();
                f.write_all(name.as_bytes()).unwrap();
            }
        }
        let mut fs = Fat32::mount(disk).unwrap();
        let mut out: [Entry; 48] = core::array::from_fn(|_| Entry { cluster:0, size:0, is_dir:false, name:[0;12], name_len:0 });
        let count = fs.read_dir(fs.root_cluster, &mut out);
        assert_eq!(count, 3);
    }

    // ── read_file ────────────────────────────────────────────────────────────

    #[test]
    fn read_file_small() {
        let content = b"Hello, FAT32!";
        let disk = disk_with_file("TEST.TXT", content);
        let mut fs = Fat32::mount(disk).unwrap();
        let entry = fs.lookup("TEST.TXT").unwrap();
        let mut buf = [0u8; 64];
        let n = fs.read_file(entry.cluster, entry.size, buf.as_mut_ptr());
        assert_eq!(n, content.len());
        assert_eq!(&buf[..n], content);
    }

    #[test]
    fn read_file_large() {
        // Write a file larger than one sector (512 bytes)
        let content: std::vec::Vec<u8> = (0..2000_u32).map(|i| (i & 0xFF) as u8).collect();
        let disk = disk_with_file("BIG.BIN", &content);
        let mut fs = Fat32::mount(disk).unwrap();
        let entry = fs.lookup("BIG.BIN").unwrap();
        let mut buf = vec![0u8; content.len()];
        let n = fs.read_file(entry.cluster, entry.size, buf.as_mut_ptr());
        assert_eq!(n, content.len());
        assert_eq!(buf, content);
    }

    #[test]
    fn read_file_multi_cluster() {
        // Write a file that spans multiple clusters (default cluster = 8 sectors = 4 KB)
        let content: std::vec::Vec<u8> = (0..10_000_u32).map(|i| (i ^ 0xAB) as u8).collect();
        let disk = disk_with_file("MULTI.BIN", &content);
        let mut fs = Fat32::mount(disk).unwrap();
        let entry = fs.lookup("MULTI.BIN").unwrap();
        let mut buf = vec![0u8; content.len()];
        let n = fs.read_file(entry.cluster, entry.size, buf.as_mut_ptr());
        assert_eq!(n, content.len());
        assert_eq!(buf, content);
    }

    // ── write_file ───────────────────────────────────────────────────────────

    #[test]
    fn write_file_new() {
        let mut disk = make_disk();
        {
            let d = MemDisk(disk.0.clone());
            let mut fs = Fat32::mount(d).unwrap();
            assert!(fs.write_file("OUT.TXT", b"written by fat32"));
            disk.0 = fs.disk.0;
        }
        let result = read_via_fatfs(&mut disk, "OUT.TXT");
        assert_eq!(result, b"written by fat32");
    }

    #[test]
    fn write_file_overwrite() {
        let mut disk = disk_with_file("OVER.TXT", b"original");
        {
            let d = MemDisk(disk.0.clone());
            let mut fs = Fat32::mount(d).unwrap();
            assert!(fs.write_file("OVER.TXT", b"replaced"));
            disk.0 = fs.disk.0;
        }
        let result = read_via_fatfs(&mut disk, "OVER.TXT");
        assert_eq!(result, b"replaced");
    }

    #[test]
    fn write_file_roundtrip_large() {
        let content: std::vec::Vec<u8> = (0..8192_u32).map(|i| (i * 7) as u8).collect();
        let mut disk = make_disk();
        {
            let d = MemDisk(disk.0.clone());
            let mut fs = Fat32::mount(d).unwrap();
            assert!(fs.write_file("LARGE.BIN", &content));
            disk.0 = fs.disk.0;
        }
        let result = read_via_fatfs(&mut disk, "LARGE.BIN");
        assert_eq!(result, content);
    }

    #[test]
    fn write_then_read_via_fat32() {
        let content = b"fat32 roundtrip";
        let disk = make_disk();
        let d = MemDisk(disk.0.clone());
        let mut fs = Fat32::mount(d).unwrap();
        assert!(fs.write_file("ROUND.TXT", content));
        let entry = fs.lookup("ROUND.TXT").unwrap();
        let mut buf = [0u8; 64];
        let n = fs.read_file(entry.cluster, entry.size, buf.as_mut_ptr());
        assert_eq!(&buf[..n], content as &[u8]);
    }
}
