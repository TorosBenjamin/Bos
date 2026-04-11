/// Tests for the shared buffer lifecycle (create → map → munmap → destroy).
///
/// The bug being guarded: `sys_munmap` used to free SharedBuf frames as
/// `UsedByUserMode`, then `sys_destroy_shared_buf` freed them again as
/// `SharedBuffer` — corrupting the frame allocator.  These tests catch that
/// by allocating frames after the lifecycle and asserting no duplicates.
use crate::TestResult;
use alloc::format;
use core::sync::atomic::Ordering;

// For Cr3::write we need x86_64 0.15.4 (the tests-crate dependency, same
// version the other test modules use).
use x86_64::instructions::interrupts;
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::{PhysFrame, Size4KiB};
use x86_64::PhysAddr;

// For kernel physical memory APIs we use the re-exported (git) version.
use kernel::memory::physical_memory::MemoryType;
use kernel::reexports::x86_64::structures::paging::{
    PhysFrame as KPhysFrame, Size4KiB as KSize4KiB,
};
use kernel::reexports::x86_64::PhysAddr as KPhysAddr;

use kernel::memory::cpu_local_data::get_local;
use kernel::memory::MEMORY;
use kernel::user_task_from_elf::create_user_task_from_elf_bytes;
use kernel_api_types::Priority;

// ─── helper (mirrors syscalls::with_user_context) ───────────────────────────

fn get_init_task_elf() -> &'static [u8] {
    use core::ptr::{slice_from_raw_parts_mut, NonNull};
    let module = kernel::limine_requests::MODULE_REQUEST
        .get_response()
        .unwrap()
        .modules()
        .iter()
        .find(|m| m.path() == kernel::limine_requests::INIT_TASK_PATH)
        .expect("init_task module not found");
    let ptr =
        NonNull::new(slice_from_raw_parts_mut(module.addr(), module.size() as usize)).unwrap();
    unsafe { ptr.as_ref() }
}

fn with_user_context(f: impl FnOnce() -> TestResult) -> TestResult {
    use alloc::sync::Arc;
    let task = match create_user_task_from_elf_bytes(
        get_init_task_elf(), 0, b"", Priority::Normal, None,
    ) {
        Ok(t) => Arc::new(t),
        Err(e) => return TestResult::Failed(format!("task creation failed: {e:?}")),
    };

    interrupts::without_interrupts(|| {
        {
            let cpu = get_local();
            let mut rq = cpu.run_queue.get().unwrap().lock();
            rq.current_task = Some(task.clone());
        }

        let (kernel_frame, cr3_flags) = Cr3::read();
        let user_frame = PhysFrame::<Size4KiB>::containing_address(
            PhysAddr::new(task.cr3.load(Ordering::Relaxed)),
        );
        unsafe { Cr3::write(user_frame, cr3_flags) };

        let result = f();

        unsafe { Cr3::write(kernel_frame, cr3_flags) };
        {
            let cpu = get_local();
            let mut rq = cpu.run_queue.get().unwrap().lock();
            rq.current_task = None;
        }
        result
    })
}

/// Allocate `n` frames and return an error if any address appears twice.
/// Frees all allocated frames before returning.
fn check_no_duplicate_frames(n: usize) -> TestResult {
    let mem = MEMORY.get().unwrap();
    let mut phys = mem.physical_memory.lock();
    let mut addrs = heapless::Vec::<u64, 32>::new();

    for _ in 0..n {
        let frame = match phys.allocate_frame_with_type(MemoryType::UsedByUserMode) {
            Some(f) => f,
            None => break,
        };
        let addr = frame.start_address().as_u64();
        if addrs.contains(&addr) {
            for &a in &addrs {
                let f = KPhysFrame::<KSize4KiB>::from_start_address(KPhysAddr::new(a)).unwrap();
                let _ = phys.free_frame(f, MemoryType::UsedByUserMode);
            }
            let _ = phys.free_frame(frame, MemoryType::UsedByUserMode);
            return TestResult::Failed(format!(
                "duplicate frame {addr:#x} — frame allocator corrupted (double-free)"
            ));
        }
        let _ = addrs.push(addr);
    }

    for &a in &addrs {
        let f = KPhysFrame::<KSize4KiB>::from_start_address(KPhysAddr::new(a)).unwrap();
        let _ = phys.free_frame(f, MemoryType::UsedByUserMode);
    }
    TestResult::Ok
}

// ─── tests ───────────────────────────────────────────────────────────────────

/// Basic: create → write → munmap (creator) → destroy.
/// Verifies the buf_id is valid, the vaddr lands in user space, and data can be
/// read back from the creator mapping.
pub fn test_shared_buf_create_and_destroy() -> TestResult {
    with_user_context(|| {
        use kernel_api_types::MMAP_WRITE;

        let out_buf = kernel::syscall_handlers::sys_mmap(8, MMAP_WRITE, 0, 0, 0, 0);
        if out_buf == 0 {
            return TestResult::Failed("sys_mmap for vaddr_out failed".into());
        }
        let task = {
            let cpu = get_local();
            let rq = cpu.run_queue.get().unwrap().lock();
            rq.current_task.clone().unwrap()
        };
        if !kernel::memory::demand::prefault_user_range(&task, out_buf, out_buf + 8) {
            return TestResult::Failed("prefault of vaddr_out failed".into());
        }

        let buf_id =
            kernel::syscall_handlers::sys_create_shared_buf(4096, out_buf, 0, 0, 0, 0);
        if buf_id == u64::MAX {
            return TestResult::Failed("sys_create_shared_buf failed".into());
        }
        let vaddr = unsafe { core::ptr::read(out_buf as *const u64) };

        if vaddr == 0 || !vaddr.is_multiple_of(4096) {
            return TestResult::Failed(format!("bad vaddr {vaddr:#x}"));
        }
        if !(kernel::consts::USER_MIN..=kernel::consts::USER_MAX).contains(&vaddr) {
            return TestResult::Failed(format!("vaddr {vaddr:#x} outside user range"));
        }

        // Write/read through the creator's mapping
        unsafe { core::ptr::write(vaddr as *mut u64, 0xABCD_1234_5678_EFu64) };
        let readback = unsafe { core::ptr::read(vaddr as *const u64) };
        if readback != 0xABCD_1234_5678_EFu64 {
            return TestResult::Failed(format!("shared buf r/w mismatch: {readback:#x}"));
        }

        let mu = kernel::syscall_handlers::sys_munmap(vaddr, 4096, 0, 0, 0, 0);
        if mu != 0 {
            return TestResult::Failed(format!("sys_munmap returned {mu:#x}"));
        }
        kernel::syscall_handlers::sys_destroy_shared_buf(buf_id, 0, 0, 0, 0, 0);

        TestResult::Ok
    })
}

/// Regression test for the SharedBuf double-free bug.
///
/// Full lifecycle: create → creator-munmap → client-map → client-munmap → destroy.
/// After that, verifies the physical frame allocator produces no duplicate
/// addresses.  Before the fix, `sys_munmap` freed the SharedBuf frames and
/// `sys_destroy_shared_buf` freed them again, corrupting the allocator so the
/// next two allocations could return the same frame.
pub fn test_shared_buf_munmap_does_not_double_free() -> TestResult {
    with_user_context(|| {
        use kernel_api_types::MMAP_WRITE;

        let out_buf = kernel::syscall_handlers::sys_mmap(8, MMAP_WRITE, 0, 0, 0, 0);
        if out_buf == 0 {
            return TestResult::Failed("sys_mmap for vaddr_out failed".into());
        }
        let task = {
            let cpu = get_local();
            let rq = cpu.run_queue.get().unwrap().lock();
            rq.current_task.clone().unwrap()
        };
        if !kernel::memory::demand::prefault_user_range(&task, out_buf, out_buf + 8) {
            return TestResult::Failed("prefault failed".into());
        }

        // 4 pages so we have several frames to stress-check
        let buf_id =
            kernel::syscall_handlers::sys_create_shared_buf(4 * 4096, out_buf, 0, 0, 0, 0);
        if buf_id == u64::MAX {
            return TestResult::Failed("sys_create_shared_buf failed".into());
        }
        let creator_vaddr = unsafe { core::ptr::read(out_buf as *const u64) };

        // Simulate the client mapping the same buffer
        let client_vaddr =
            kernel::syscall_handlers::sys_map_shared_buf(buf_id, 0, 0, 0, 0, 0);
        if client_vaddr == 0 {
            return TestResult::Failed("sys_map_shared_buf failed".into());
        }

        // Both mappings must see the same physical memory
        unsafe { core::ptr::write(creator_vaddr as *mut u64, 0xDEAD_BEEF_CAFE_BABEu64) };
        let seen_by_client = unsafe { core::ptr::read(client_vaddr as *const u64) };
        if seen_by_client != 0xDEAD_BEEF_CAFE_BABEu64 {
            return TestResult::Failed(format!(
                "shared memory mismatch: client saw {seen_by_client:#x}"
            ));
        }

        // Unmap both copies, then destroy — before the fix this double-freed frames
        let r1 = kernel::syscall_handlers::sys_munmap(creator_vaddr, 4 * 4096, 0, 0, 0, 0);
        let r2 = kernel::syscall_handlers::sys_munmap(client_vaddr, 4 * 4096, 0, 0, 0, 0);
        if r1 != 0 || r2 != 0 {
            return TestResult::Failed(format!("munmap failed: {r1:#x} / {r2:#x}"));
        }
        kernel::syscall_handlers::sys_destroy_shared_buf(buf_id, 0, 0, 0, 0, 0);

        // A double-free puts the same frame twice in the free list; the next two
        // allocations both return it.  Checking 16 frames catches this reliably.
        check_no_duplicate_frames(16)
    })
}

/// Stress: repeat the full lifecycle 32 times.
/// Any crash or duplicate frame within the loop indicates allocator corruption.
pub fn test_shared_buf_lifecycle_stress() -> TestResult {
    with_user_context(|| {
        use kernel_api_types::MMAP_WRITE;

        let out_buf = kernel::syscall_handlers::sys_mmap(8, MMAP_WRITE, 0, 0, 0, 0);
        if out_buf == 0 {
            return TestResult::Failed("sys_mmap for vaddr_out failed".into());
        }
        let task = {
            let cpu = get_local();
            let rq = cpu.run_queue.get().unwrap().lock();
            rq.current_task.clone().unwrap()
        };
        if !kernel::memory::demand::prefault_user_range(&task, out_buf, out_buf + 8) {
            return TestResult::Failed("prefault failed".into());
        }

        for i in 0..32u32 {
            let buf_id =
                kernel::syscall_handlers::sys_create_shared_buf(2 * 4096, out_buf, 0, 0, 0, 0);
            if buf_id == u64::MAX {
                return TestResult::Failed(format!("iteration {i}: create failed"));
            }
            let creator_vaddr = unsafe { core::ptr::read(out_buf as *const u64) };

            let client_vaddr =
                kernel::syscall_handlers::sys_map_shared_buf(buf_id, 0, 0, 0, 0, 0);
            if client_vaddr == 0 {
                return TestResult::Failed(format!("iteration {i}: map failed"));
            }

            let r1 =
                kernel::syscall_handlers::sys_munmap(creator_vaddr, 2 * 4096, 0, 0, 0, 0);
            let r2 =
                kernel::syscall_handlers::sys_munmap(client_vaddr, 2 * 4096, 0, 0, 0, 0);
            if r1 != 0 || r2 != 0 {
                return TestResult::Failed(format!(
                    "iteration {i}: munmap failed: {r1:#x} / {r2:#x}"
                ));
            }
            kernel::syscall_handlers::sys_destroy_shared_buf(buf_id, 0, 0, 0, 0, 0);
        }

        check_no_duplicate_frames(16)
    })
}
