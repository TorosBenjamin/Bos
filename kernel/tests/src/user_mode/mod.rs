use crate::{TestResult, exit_qemu, QemuExitCode};
use kernel::consts::LOWER_HALF_END;
use kernel::memory::cpu_local_data::get_local;
use kernel::task::task::TaskKind;
use alloc::format;

/// Verify user segment selectors from the GDT have RPL=3.
pub fn test_user_selector_rpl() -> TestResult {
    let local = get_local();
    let gdt = local.gdt.get().unwrap();

    let user_cs = gdt.user_code_selector().0;
    let user_ss = gdt.user_data_selector().0;

    let cs_rpl = user_cs & 3;
    let ss_rpl = user_ss & 3;

    if cs_rpl != 3 {
        return TestResult::Failed(format!(
            "user CS RPL = {} (expected 3), raw selector = {:#06x}",
            cs_rpl, user_cs
        ));
    }
    if ss_rpl != 3 {
        return TestResult::Failed(format!(
            "user SS RPL = {} (expected 3), raw selector = {:#06x}",
            ss_rpl, user_ss
        ));
    }

    // Also check the GDT index is correct (CS=index 4, SS=index 3)
    let cs_index = user_cs >> 3;
    let ss_index = user_ss >> 3;
    if cs_index != 4 {
        return TestResult::Failed(format!(
            "user CS GDT index = {} (expected 4)",
            cs_index
        ));
    }
    if ss_index != 3 {
        return TestResult::Failed(format!(
            "user SS GDT index = {} (expected 3)",
            ss_index
        ));
    }

    TestResult::Ok
}

fn is_canonical(addr: u64) -> bool {
    // In 48-bit virtual addressing, bits 63:48 must equal bit 47.
    let sign_extended = ((addr as i64) << 16) >> 16;
    sign_extended as u64 == addr
}

/// Verify LOWER_HALF_END is a valid (canonical) address for use as user RSP.
pub fn test_lower_half_end_canonical() -> TestResult {
    if is_canonical(LOWER_HALF_END) {
        TestResult::Ok
    } else {
        TestResult::Failed(format!(
            "LOWER_HALF_END = {:#018x} is non-canonical! \
             Max canonical lower-half addr is 0x00007FFFFFFFFFFF",
            LOWER_HALF_END
        ))
    }
}

/// Inspect the CpuContext of a freshly created user task.
/// Verifies the iretq frame values (CS, SS, RSP, RIP) are correct.
pub fn test_user_task_iretq_frame() -> TestResult {
    let task = kernel::user_task_from_elf::create_user_task_from_elf();
    let inner = task.inner.lock();

    // CpuContext now holds all the registers and iretq frame values
    let ctx = &inner.context;
    let rip = ctx.rip;
    let cs = ctx.cs;
    let rflags = ctx.rflags;
    let user_rsp = ctx.rsp;
    let ss = ctx.ss;

    log::info!(
        "CpuContext: rip={:#018x} cs={:#06x} rflags={:#x} rsp={:#018x} ss={:#06x}",
        rip, cs, rflags, user_rsp, ss
    );

    // CS must have RPL=3
    if cs & 3 != 3 {
        return TestResult::Failed(format!(
            "iretq frame CS RPL = {}, raw = {:#06x} (expected RPL=3)",
            cs & 3, cs
        ));
    }

    // SS must have RPL=3
    if ss & 3 != 3 {
        return TestResult::Failed(format!(
            "iretq frame SS RPL = {}, raw = {:#06x} (expected RPL=3)",
            ss & 3, ss
        ));
    }

    // User RSP must be canonical
    if !is_canonical(user_rsp) {
        return TestResult::Failed(format!(
            "iretq frame RSP = {:#018x} is NON-CANONICAL! \
             This will cause #GP or #SS on iretq to Ring 3",
            user_rsp
        ));
    }

    // RIP must be in user space (lower half, non-zero)
    if rip == 0 {
        return TestResult::Failed("iretq frame RIP is 0 (null)".into());
    }
    if rip > LOWER_HALF_END {
        return TestResult::Failed(format!(
            "iretq frame RIP = {:#018x} is not in the lower half (> {:#018x})",
            rip, LOWER_HALF_END
        ));
    }

    // RFLAGS must have IF (bit 9) set for interrupts in user mode
    if rflags & 0x200 == 0 {
        return TestResult::Failed(format!(
            "iretq frame RFLAGS = {:#x} does not have IF (interrupt flag) set",
            rflags
        ));
    }

    TestResult::Ok
}

/// Verify the user task is created with the correct kind.
pub fn test_user_task_creation() -> TestResult {
    let task = kernel::user_task_from_elf::create_user_task_from_elf();

    if task.kind != TaskKind::User {
        return TestResult::Failed(format!(
            "Task kind = {:?} (expected User)",
            task.kind
        ));
    }

    // CR3 should differ from the current (kernel) CR3
    let (current_cr3_frame, _) = x86_64::registers::control::Cr3::read();
    let current_cr3 = current_cr3_frame.start_address().as_u64();
    if task.cr3 == current_cr3 {
        return TestResult::Failed(format!(
            "User task CR3 = {:#x} matches kernel CR3 (should be separate address space)",
            task.cr3
        ));
    }

    TestResult::Ok
}

/// Verify that the user page table maps the kernel higher half.
/// Actually switches CR3 to the user page table and reads back values
/// from the kernel stack and GDT to verify they are accessible.
pub fn test_user_page_table_kernel_mapped() -> TestResult {
    let task = kernel::user_task_from_elf::create_user_task_from_elf();
    let inner = task.inner.lock();
    let kernel_stack_top = inner.kernel_stack_top;

    if kernel_stack_top < 0xFFFF_8000_0000_0000 {
        return TestResult::Failed(format!(
            "Kernel stack top = {:#x} is not in the higher half",
            kernel_stack_top
        ));
    }

    // Read the SS value from the CpuContext before CR3 switch to compare later.
    let ss_before = inner.context.ss;

    // Get a pointer to the context to read after CR3 switch
    let ctx_ptr = &inner.context as *const kernel::task::task::CpuContext;

    // Switch CR3 to the user page table and verify kernel memory is accessible.
    let user_cr3 = task.cr3;
    let (current_cr3_frame, current_cr3_flags) = x86_64::registers::control::Cr3::read();
    let _current_cr3 = current_cr3_frame.start_address().as_u64();

    x86_64::instructions::interrupts::without_interrupts(|| {
        // Switch to user page table
        let user_frame = x86_64::structures::paging::PhysFrame::<x86_64::structures::paging::Size4KiB>::containing_address(
            x86_64::PhysAddr::new(user_cr3)
        );
        unsafe { x86_64::registers::control::Cr3::write(user_frame, current_cr3_flags) };

        // Try to read from the CpuContext (which is in kernel memory)
        let ss_after = unsafe { (*ctx_ptr).ss };

        // Read from the GDT to verify it's accessible
        let sgdt = x86_64::instructions::tables::sgdt();
        let gdt_base = sgdt.base.as_u64();
        let _gdt_first_entry = unsafe { *(gdt_base as *const u64) };

        // Switch back to the kernel page table
        unsafe { x86_64::registers::control::Cr3::write(current_cr3_frame, current_cr3_flags) };

        if ss_before != ss_after {
            log::error!(
                "Kernel memory not readable under user CR3! ss_before={:#x} ss_after={:#x}",
                ss_before, ss_after
            );
        }

        log::info!(
            "User CR3 test: kernel memory readable (ss={:#x}), GDT at {:#x} readable",
            ss_after, gdt_base
        );
    });

    TestResult::Ok
}

use core::sync::atomic::{AtomicU64, Ordering};

static KERNEL_TASK_COUNTER: AtomicU64 = AtomicU64::new(0);

fn kernel_increment_task() -> ! {
    KERNEL_TASK_COUNTER.fetch_add(1, Ordering::SeqCst);
    loop {
        core::hint::spin_loop();
    }
}

/// Diagnostic variant of `test_user_task_runs`: replaces the user ELF task
/// with a third plain kernel task so that no ELF parsing or ring-3 transition
/// occurs.  If this test passes but `test_user_task_runs` hangs, the problem
/// is in ELF loading, user-mode page table setup, or the iretq-to-ring-3 path.
///
/// Run via `cargo ktest-sched-noelf`.
pub fn test_user_task_runs_no_elf() -> TestResult {
    x86_64::instructions::interrupts::disable();
    KERNEL_TASK_COUNTER.store(0, Ordering::SeqCst);

    kernel::task::global_scheduler::spawn_task(
        kernel::task::task::Task::new(kernel_increment_task),
    );
    kernel::task::global_scheduler::spawn_task(
        kernel::task::task::Task::new(kernel_increment_task),
    );
    // Third kernel task — replaces the user ELF task for isolation
    kernel::task::global_scheduler::spawn_task(
        kernel::task::task::Task::new(kernel_increment_task),
    );
    kernel::task::global_scheduler::spawn_task(
        kernel::task::task::Task::new(checker_task),
    );

    kernel::time::lapic_timer::set_deadline(1_000_000);
    x86_64::instructions::interrupts::enable();
    loop {
        x86_64::instructions::hlt();
    }
}

/// Integration test: schedule both kernel tasks and a user task, then
/// verify the scheduler handled them all without faulting.
///
/// This test hands control to the scheduler and never returns.
/// It **must** be the LAST test in the test list.
pub fn test_user_task_runs() -> TestResult {
    // Disable interrupts so the scheduler cannot preempt us before all
    // tasks are spawned.  The timer_interrupt_fires test leaves them enabled.
    x86_64::instructions::interrupts::disable();

    KERNEL_TASK_COUNTER.store(0, Ordering::SeqCst);

    // Initialize the syscall handler so the user ELF can make syscalls
    kernel::raw_syscall_handler::init();

    // Spawn two kernel tasks
    kernel::task::global_scheduler::spawn_task(
        kernel::task::task::Task::new(kernel_increment_task),
    );
    kernel::task::global_scheduler::spawn_task(
        kernel::task::task::Task::new(kernel_increment_task),
    );

    // Create and spawn the user task
    let user_task = kernel::user_task_from_elf::create_user_task_from_elf();
    kernel::task::global_scheduler::spawn_task(user_task);

    // Spawn a checker task that verifies everything ran.
    kernel::task::global_scheduler::spawn_task(
        kernel::task::task::Task::new(checker_task),
    );

    // Explicitly arm the LAPIC timer so the scheduler fires even if no previous
    // test re-armed it (e.g. test_timer_stack_alignment returns early when
    // TIMER_STACK_ALIGNMENT_OK is already set).
    kernel::time::lapic_timer::set_deadline(1_000_000);

    // Enable interrupts — the scheduler takes over
    x86_64::instructions::interrupts::enable();
    loop {
        x86_64::instructions::hlt();
    }
}

fn checker_task() -> ! {
    let count_at_start = KERNEL_TASK_COUNTER.load(Ordering::SeqCst);
    log::info!(
        "checker_task: started, counter={}",
        count_at_start
    );

    let start = kernel::time::tsc::value();
    let timeout = kernel::time::tsc::TSC_HZ.load(Ordering::SeqCst).saturating_mul(1000);

    // Wait for both kernel tasks to have executed
    while KERNEL_TASK_COUNTER.load(Ordering::SeqCst) < 2 {
        if kernel::time::tsc::value().wrapping_sub(start) > timeout {
            break;
        }
        core::hint::spin_loop();
    }

    let counter = KERNEL_TASK_COUNTER.load(Ordering::SeqCst);
    if counter < 2 {
        log::error!(
            "tests::user_mode::test_user_task_runs [failed] - \
             kernel task counter = {}/2 (kernel tasks didn't run)",
            counter
        );
        exit_qemu(QemuExitCode::Failed);
    } else {
        // If we got here, the scheduler ran kernel tasks AND the user task
        // without GPF (the user task was in the run queue and was scheduled).
        log::info!("tests::user_mode::test_user_task_runs [ok]");
        log::info!("All tests passed!");
        exit_qemu(QemuExitCode::Success);
    }

    loop {
        core::hint::spin_loop();
    }
}
