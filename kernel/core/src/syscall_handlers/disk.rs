use crate::drivers::disk;

/// Syscall: read sectors from the IDE disk into a user-provided buffer.
///
/// args: lba (u64), count (u64), buf_ptr (u64)
/// returns: 1 on success, 0 on failure.
pub fn sys_block_read_sectors(lba: u64, count: u64, buf_ptr: u64, _: u64, _: u64, _: u64) -> u64 {
    if count == 0 || count > 256 {
        return 0;
    }
    let byte_count = count * 512;
    if !super::validate_user_ptr(buf_ptr, byte_count) {
        return 0;
    }
    if !disk::DISK_PRESENT.load(core::sync::atomic::Ordering::Acquire) {
        return 0;
    }
    let buf = unsafe { core::slice::from_raw_parts_mut(buf_ptr as *mut u8, byte_count as usize) };
    if disk::read_sectors(lba, count as u32, buf) { 1 } else { 0 }
}

/// Syscall: write sectors to the IDE disk from a user-provided buffer.
///
/// args: lba (u64), count (u64), buf_ptr (u64)
/// returns: 1 on success, 0 on failure.
pub fn sys_block_write_sectors(lba: u64, count: u64, buf_ptr: u64, _: u64, _: u64, _: u64) -> u64 {
    if count == 0 || count > 256 {
        return 0;
    }
    let byte_count = count * 512;
    if !super::validate_user_ptr(buf_ptr, byte_count) {
        return 0;
    }
    if !disk::DISK_PRESENT.load(core::sync::atomic::Ordering::Acquire) {
        return 0;
    }
    let buf = unsafe { core::slice::from_raw_parts(buf_ptr as *const u8, byte_count as usize) };
    if disk::write_sectors(lba, count as u32, buf) { 1 } else { 0 }
}
