use crate::TestResult;
use alloc::format;
use alloc::vec::Vec;
use elf::ElfBytes;
use elf::endian::AnyEndian;
use elf::abi::PT_LOAD;

// ── helpers ────────────────────────────────────────────────────────────────

fn get_module_bytes(path: &[u8]) -> Option<&'static [u8]> {
    use core::ptr::{NonNull, slice_from_raw_parts_mut};
    let response = kernel::limine_requests::MODULE_REQUEST.get_response()?;
    let module = response.modules().iter().find(|m| m.path().to_bytes() == path)?;
    let ptr = NonNull::new(slice_from_raw_parts_mut(module.addr(), module.size() as usize))?;
    Some(unsafe { ptr.as_ref() })
}

/// Parse an ELF from module bytes or return a descriptive failure.
fn parse_elf(path: &[u8]) -> Result<(&'static [u8], ElfBytes<'static, AnyEndian>), TestResult> {
    let bytes = match get_module_bytes(path) {
        Some(b) => b,
        None => return Err(TestResult::Failed(format!(
            "module {:?} not found",
            core::str::from_utf8(path).unwrap_or("?")
        ))),
    };
    match ElfBytes::<AnyEndian>::minimal_parse(bytes) {
        Ok(elf) => Ok((bytes, elf)),
        Err(e) => Err(TestResult::Failed(format!(
            "ELF parse error for {:?}: {e}",
            core::str::from_utf8(path).unwrap_or("?")
        ))),
    }
}

// ── tests ──────────────────────────────────────────────────────────────────

/// ELF magic, class (64-bit), data (little-endian), type (ET_EXEC), machine
/// (EM_X86_64) must all be valid for both init_task and display_server.
pub fn test_elf_header_valid() -> TestResult {
    for path in [b"/init_task".as_slice(), b"/display_server".as_slice()] {
        let (bytes, elf) = match parse_elf(path) {
            Ok(t) => t,
            Err(r) => return r,
        };

        if bytes.len() < 16 || &bytes[0..4] != b"\x7fELF" {
            return TestResult::Failed(format!(
                "{}: bad ELF magic",
                core::str::from_utf8(path).unwrap_or("?")
            ));
        }
        // EI_CLASS = 2 (ELFCLASS64)
        if bytes[4] != 2 {
            return TestResult::Failed(format!(
                "{}: EI_CLASS {} (expected 2=64-bit)",
                core::str::from_utf8(path).unwrap_or("?"),
                bytes[4]
            ));
        }
        // EI_DATA = 1 (ELFDATA2LSB, little-endian)
        if bytes[5] != 1 {
            return TestResult::Failed(format!(
                "{}: EI_DATA {} (expected 1=little-endian)",
                core::str::from_utf8(path).unwrap_or("?"),
                bytes[5]
            ));
        }
        // e_type = 2 (ET_EXEC)
        if elf.ehdr.e_type != 2 {
            return TestResult::Failed(format!(
                "{}: e_type {} (expected 2=ET_EXEC)",
                core::str::from_utf8(path).unwrap_or("?"),
                elf.ehdr.e_type
            ));
        }
        // e_machine = 62 (EM_X86_64)
        if elf.ehdr.e_machine != 62 {
            return TestResult::Failed(format!(
                "{}: e_machine {} (expected 62=EM_X86_64)",
                core::str::from_utf8(path).unwrap_or("?"),
                elf.ehdr.e_machine
            ));
        }
    }
    TestResult::Ok
}

/// Every ELF module must have at least one PT_LOAD segment.
pub fn test_elf_has_load_segments() -> TestResult {
    for path in [b"/init_task".as_slice(), b"/display_server".as_slice()] {
        let (_, elf) = match parse_elf(path) {
            Ok(t) => t,
            Err(r) => return r,
        };
        let segments = match elf.segments() {
            Some(s) => s,
            None => return TestResult::Failed(format!(
                "{}: ELF has no segment table",
                core::str::from_utf8(path).unwrap_or("?")
            )),
        };
        let count = segments.iter().filter(|s| s.p_type == PT_LOAD).count();
        if count == 0 {
            return TestResult::Failed(format!(
                "{}: no PT_LOAD segments",
                core::str::from_utf8(path).unwrap_or("?")
            ));
        }
    }
    TestResult::Ok
}

/// The ELF entry point must be non-zero, in the user lower half
/// (≤ 0x7FFF_FFFF_FFFF), and must fall within some PT_LOAD segment's
/// [p_vaddr, p_vaddr + p_memsz) range.
///
/// A fault at ip=0x8 (or any near-null address) means the entry is wrong.
/// This test would catch that before the task is even scheduled.
pub fn test_elf_entry_in_load_segment() -> TestResult {
    for path in [b"/init_task".as_slice(), b"/display_server".as_slice()] {
        let (_, elf) = match parse_elf(path) {
            Ok(t) => t,
            Err(r) => return r,
        };
        let entry = elf.ehdr.e_entry;

        if entry == 0 {
            return TestResult::Failed(format!(
                "{}: e_entry is 0 (null entry point)",
                core::str::from_utf8(path).unwrap_or("?")
            ));
        }
        // Must be in canonical lower half
        if entry > 0x0000_7FFF_FFFF_FFFF {
            return TestResult::Failed(format!(
                "{}: e_entry {:#x} is outside the lower half",
                core::str::from_utf8(path).unwrap_or("?"),
                entry
            ));
        }
        // Must lie within a LOAD segment
        let segments = elf.segments().unwrap();
        let in_load = segments
            .iter()
            .filter(|s| s.p_type == PT_LOAD)
            .any(|s| entry >= s.p_vaddr && entry < s.p_vaddr + s.p_memsz);

        if !in_load {
            return TestResult::Failed(format!(
                "{}: e_entry {:#x} is not inside any PT_LOAD segment \
                 (this would cause an immediate page fault on task start)",
                core::str::from_utf8(path).unwrap_or("?"),
                entry
            ));
        }
    }
    TestResult::Ok
}

/// PT_LOAD file data must be entirely within the module bytes and
/// p_filesz must not exceed p_memsz.
pub fn test_elf_segment_file_bounds() -> TestResult {
    for path in [b"/init_task".as_slice(), b"/display_server".as_slice()] {
        let (bytes, elf) = match parse_elf(path) {
            Ok(t) => t,
            Err(r) => return r,
        };
        let segments = match elf.segments() {
            Some(s) => s,
            None => return TestResult::Failed(format!(
                "{}: no segment table",
                core::str::from_utf8(path).unwrap_or("?")
            )),
        };
        for seg in segments.iter().filter(|s| s.p_type == PT_LOAD) {
            if seg.p_filesz > seg.p_memsz {
                return TestResult::Failed(format!(
                    "{}: PT_LOAD p_filesz {:#x} > p_memsz {:#x}",
                    core::str::from_utf8(path).unwrap_or("?"),
                    seg.p_filesz,
                    seg.p_memsz
                ));
            }
            let end = seg.p_offset.saturating_add(seg.p_filesz);
            if end > bytes.len() as u64 {
                return TestResult::Failed(format!(
                    "{}: PT_LOAD file data [{:#x}, {:#x}) exceeds module size {:#x}",
                    core::str::from_utf8(path).unwrap_or("?"),
                    seg.p_offset,
                    end,
                    bytes.len()
                ));
            }
        }
    }
    TestResult::Ok
}

/// PT_LOAD virtual address ranges must not overlap each other.
pub fn test_elf_load_segments_no_overlap() -> TestResult {
    for path in [b"/init_task".as_slice(), b"/display_server".as_slice()] {
        let (_, elf) = match parse_elf(path) {
            Ok(t) => t,
            Err(r) => return r,
        };
        let segments = match elf.segments() {
            Some(s) => s,
            None => return TestResult::Failed(format!(
                "{}: no segment table",
                core::str::from_utf8(path).unwrap_or("?")
            )),
        };
        let mut ranges: Vec<(u64, u64)> = Vec::new();
        for seg in segments.iter().filter(|s| s.p_type == PT_LOAD) {
            let start = seg.p_vaddr;
            let end = seg.p_vaddr + seg.p_memsz;
            for &(prev_start, prev_end) in &ranges {
                if start < prev_end && end > prev_start {
                    return TestResult::Failed(format!(
                        "{}: PT_LOAD vaddr ranges overlap: \
                         [{:#x},{:#x}) and [{:#x},{:#x})",
                        core::str::from_utf8(path).unwrap_or("?"),
                        prev_start, prev_end, start, end
                    ));
                }
            }
            ranges.push((start, end));
        }
    }
    TestResult::Ok
}

/// After `create_user_task_from_elf_bytes`, the task's RIP must equal
/// the ELF's e_entry and must be in user space.
pub fn test_spawn_rip_matches_elf_entry() -> TestResult {
    let (bytes, elf) = match parse_elf(b"/init_task") {
        Ok(t) => t,
        Err(r) => return r,
    };
    let expected_entry = elf.ehdr.e_entry;

    let task = match kernel::user_task_from_elf::create_user_task_from_elf_bytes(bytes, 0) {
        Ok(t) => t,
        Err(e) => {
            return TestResult::Failed(format!(
                "create_user_task_from_elf_bytes failed: {:?}",
                e
            ))
        }
    };

    let rip = task.inner.lock().context.rip;

    if rip != expected_entry {
        return TestResult::Failed(format!(
            "task RIP {:#x} != ELF e_entry {:#x}",
            rip, expected_entry
        ));
    }
    if rip > 0x0000_7FFF_FFFF_FFFF {
        return TestResult::Failed(format!(
            "task RIP {:#x} is outside the lower half",
            rip
        ));
    }
    TestResult::Ok
}

/// After `create_user_task_from_elf()` (direct Limine module mapping),
/// the task's RIP must equal the ELF's e_entry.
pub fn test_direct_elf_entry_matches() -> TestResult {
    let (_, elf) = match parse_elf(b"/init_task") {
        Ok(t) => t,
        Err(r) => return r,
    };
    let expected_entry = elf.ehdr.e_entry;

    let task = kernel::user_task_from_elf::create_user_task_from_elf();
    let rip = task.inner.lock().context.rip;

    if rip != expected_entry {
        return TestResult::Failed(format!(
            "direct-mapped task RIP {:#x} != ELF e_entry {:#x}",
            rip, expected_entry
        ));
    }
    TestResult::Ok
}

/// After `create_user_task_from_elf_bytes`, switch to the user page table
/// and verify:
///   1. The entry point vaddr is readable (non-null bytes present).
///   2. The first 16 bytes of each PT_LOAD segment match the ELF file.
///
/// A mismatch means the ELF copy loop miscalculated page offsets.
pub fn test_spawn_data_integrity() -> TestResult {
    let (bytes, elf) = match parse_elf(b"/init_task") {
        Ok(t) => t,
        Err(r) => return r,
    };

    // Collect expected bytes for each LOAD segment (first 16 bytes of file data).
    // ELF spec guarantees p_offset % p_align == p_vaddr % p_align, so reading
    // at vaddr p_vaddr maps to ELF file offset p_offset.
    let mut checks: Vec<(u64, Vec<u8>)> = Vec::new();
    if let Some(segments) = elf.segments() {
        for seg in segments.iter().filter(|s| s.p_type == PT_LOAD && s.p_filesz > 0) {
            let off = seg.p_offset as usize;
            let sample_len = (seg.p_filesz as usize).min(16);
            let expected = bytes[off..off + sample_len].to_vec();
            checks.push((seg.p_vaddr, expected));
        }
    }

    let entry = elf.ehdr.e_entry;

    let task = match kernel::user_task_from_elf::create_user_task_from_elf_bytes(bytes, 0) {
        Ok(t) => t,
        Err(e) => {
            return TestResult::Failed(format!(
                "create_user_task_from_elf_bytes failed: {:?}",
                e
            ))
        }
    };

    let user_cr3 = task.cr3;
    let (orig_cr3_frame, orig_cr3_flags) = x86_64::registers::control::Cr3::read();

    let mut error: Option<alloc::string::String> = None;

    x86_64::instructions::interrupts::without_interrupts(|| {
        let user_frame =
            x86_64::structures::paging::PhysFrame::<x86_64::structures::paging::Size4KiB>::containing_address(
                x86_64::PhysAddr::new(user_cr3),
            );
        unsafe { x86_64::registers::control::Cr3::write(user_frame, orig_cr3_flags) };

        // 1. Entry point must have non-zero bytes (mapped and contains code).
        if error.is_none() {
            let mut buf = [0u8; 4];
            unsafe { core::ptr::copy_nonoverlapping(entry as *const u8, buf.as_mut_ptr(), 4) };
            if buf == [0, 0, 0, 0] {
                error = Some(format!(
                    "entry point {:#x} reads as all-zero (unmapped or zeroed text?)",
                    entry
                ));
            }
        }

        // 2. Segment data must match ELF file bytes.
        if error.is_none() {
            for (vaddr, expected) in &checks {
                let mut actual = [0u8; 16];
                let n = expected.len();
                unsafe { core::ptr::copy_nonoverlapping(*vaddr as *const u8, actual.as_mut_ptr(), n) };
                if &actual[..n] != expected.as_slice() {
                    error = Some(format!(
                        "data mismatch at vaddr {:#x}: \
                         expected {expected:?}, got {:?}",
                        vaddr,
                        &actual[..n]
                    ));
                    break;
                }
            }
        }

        unsafe { x86_64::registers::control::Cr3::write(orig_cr3_frame, orig_cr3_flags) };
    });

    match error {
        Some(msg) => TestResult::Failed(msg),
        None => TestResult::Ok,
    }
}

/// BSS regions (p_memsz > p_filesz in a PT_LOAD segment) must be zeroed
/// after mapping. Verified by switching to user CR3 and reading the extra bytes.
pub fn test_spawn_bss_zeroed() -> TestResult {
    let (bytes, elf) = match parse_elf(b"/init_task") {
        Ok(t) => t,
        Err(r) => return r,
    };

    // Find a segment with BSS.
    let bss_check: Option<(u64, u64)> = elf.segments().and_then(|segs| {
        segs.iter()
            .filter(|s| s.p_type == PT_LOAD && s.p_memsz > s.p_filesz)
            .map(|s| {
                // First byte of BSS: vaddr + filesz
                let bss_vaddr = s.p_vaddr + s.p_filesz;
                let bss_len = (s.p_memsz - s.p_filesz).min(64); // check up to 64 bytes
                (bss_vaddr, bss_len)
            })
            .next()
    });

    let (bss_vaddr, bss_len) = match bss_check {
        Some(v) => v,
        // No BSS — that's fine, test passes vacuously.
        None => return TestResult::Ok,
    };

    let task = match kernel::user_task_from_elf::create_user_task_from_elf_bytes(bytes, 0) {
        Ok(t) => t,
        Err(e) => {
            return TestResult::Failed(format!(
                "create_user_task_from_elf_bytes failed: {:?}",
                e
            ))
        }
    };

    let user_cr3 = task.cr3;
    let (orig_cr3_frame, orig_cr3_flags) = x86_64::registers::control::Cr3::read();

    let mut error: Option<alloc::string::String> = None;

    x86_64::instructions::interrupts::without_interrupts(|| {
        let user_frame =
            x86_64::structures::paging::PhysFrame::<x86_64::structures::paging::Size4KiB>::containing_address(
                x86_64::PhysAddr::new(user_cr3),
            );
        unsafe { x86_64::registers::control::Cr3::write(user_frame, orig_cr3_flags) };

        let mut buf = [0xFFu8; 64];
        let n = bss_len as usize;
        unsafe { core::ptr::copy_nonoverlapping(bss_vaddr as *const u8, buf.as_mut_ptr(), n) };

        if buf[..n].iter().any(|&b| b != 0) {
            error = Some(format!(
                "BSS at {:#x} (len {}) is not zeroed: {:?}",
                bss_vaddr,
                n,
                &buf[..n]
            ));
        }

        unsafe { x86_64::registers::control::Cr3::write(orig_cr3_frame, orig_cr3_flags) };
    });

    match error {
        Some(msg) => TestResult::Failed(msg),
        None => TestResult::Ok,
    }
}
