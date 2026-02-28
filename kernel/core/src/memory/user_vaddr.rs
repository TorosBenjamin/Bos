use crate::consts::{USER_MAX, USER_MIN};
use nodit::interval::ii;
use nodit::{InclusiveInterval, Interval, NoditSet};

const PAGE_SIZE: u64 = 4096;

/// Find a gap in the user vaddr set large enough for `n_pages` contiguous 4 KiB pages,
/// insert the range, and return the start virtual address.
pub fn allocate_user_pages(
    set: &mut NoditSet<u64, Interval<u64>>,
    n_pages: u64,
) -> Option<u64> {
    let total_bytes = n_pages * PAGE_SIZE;
    let range = ii(USER_MIN, USER_MAX);

    let interval = set
        .gaps_trimmed(&range)
        .find_map(|gap| {
            let aligned_start = gap.start().next_multiple_of(PAGE_SIZE);
            let end = aligned_start + total_bytes - 1;
            let interval = ii(aligned_start, end);
            gap.contains_interval(&interval).then_some(interval)
        })?;

    set.insert_merge_touching(interval).expect("no overlap");

    Some(*interval.start())
}

/// Verify that the range `[addr, addr+size)` is fully contained within the user vaddr set,
/// and remove it. Returns `true` on success.
pub fn free_user_pages(
    set: &mut NoditSet<u64, Interval<u64>>,
    addr: u64,
    size: u64,
) -> bool {
    let end = addr + size - 1;
    let interval = ii(addr, end);

    // Check that the range is fully covered by existing allocations
    let covered = set
        .iter()
        .any(|existing| existing.contains_interval(&interval));

    if !covered {
        return false;
    }

    let _ = set.cut(&interval);
    true
}

pub fn is_user_vaddr_valid_range(
    set: &NoditSet<u64, Interval<u64>>,
    start: x86_64::VirtAddr,
    end: x86_64::VirtAddr,
) -> bool {
    let interval = ii(start.as_u64(), end.as_u64() - 1);
    set.iter().any(|existing| existing.contains_interval(&interval))
}
