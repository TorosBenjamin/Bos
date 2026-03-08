use crate::consts::{USER_MAX, USER_MIN};
use crate::task::task::VmaEntry;
use nodit::interval::ii;
use nodit::{InclusiveInterval, Interval, NoditMap};
use x86_64::VirtAddr;

const PAGE_SIZE: u64 = 4096;

/// Find a free gap large enough for `n_pages` contiguous 4 KiB pages,
/// insert the VMA entry, and return the start virtual address.
pub fn allocate_user_vma(
    vmas: &mut NoditMap<u64, Interval<u64>, VmaEntry>,
    n_pages: u64,
    entry: VmaEntry,
) -> Option<u64> {
    let total_bytes = n_pages * PAGE_SIZE;
    let range = ii(USER_MIN, USER_MAX);

    let interval = vmas
        .gaps_trimmed(&range)
        .find_map(|gap| {
            let aligned_start = gap.start().next_multiple_of(PAGE_SIZE);
            let end = aligned_start + total_bytes - 1;
            let interval = ii(aligned_start, end);
            gap.contains_interval(&interval).then_some(interval)
        })?;

    let _ = vmas.insert_overwrite(interval, entry);
    Some(*interval.start())
}

/// Verify that `[addr, addr+size)` is fully covered by the VMA map (no gaps),
/// then remove it. Returns `false` if any part is uncovered.
pub fn free_user_vma(
    vmas: &mut NoditMap<u64, Interval<u64>, VmaEntry>,
    addr: u64,
    size: u64,
) -> bool {
    if size == 0 {
        return true;
    }
    let interval = ii(addr, addr + size - 1);
    // Reject if there are any gaps in the requested range.
    if vmas.gaps_trimmed(&interval).next().is_some() {
        return false;
    }
    let _ = vmas.cut(&interval).count(); // consume the iterator to drop removed entries
    true
}

/// Returns `true` if `[addr, addr+size)` has no overlap with any existing VMA.
pub fn is_range_free(
    vmas: &NoditMap<u64, Interval<u64>, VmaEntry>,
    addr: u64,
    size: u64,
) -> bool {
    if size == 0 {
        return true;
    }
    let interval = ii(addr, addr + size - 1);
    vmas.overlapping(&interval).next().is_none()
}

/// Returns `true` if `[start, end)` is fully contained within a single VMA entry.
pub fn is_user_vaddr_valid_range(
    vmas: &NoditMap<u64, Interval<u64>, VmaEntry>,
    start: VirtAddr,
    end: VirtAddr,
) -> bool {
    if start >= end {
        return false;
    }
    let interval = ii(start.as_u64(), end.as_u64() - 1);
    vmas.iter().any(|(existing, _)| existing.contains_interval(&interval))
}

/// Return the `VmaEntry` for the page containing `addr`, or `None` if unmapped.
pub fn lookup_vma(
    vmas: &NoditMap<u64, Interval<u64>, VmaEntry>,
    addr: u64,
) -> Option<VmaEntry> {
    vmas.get_at_point(&addr).copied()
}
