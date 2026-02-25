#![no_std]
#![no_main]
extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;
use kernel::hlt_loop;

pub mod panic_handler;
pub mod memory;
pub mod time;
pub mod interrupts;
pub mod graphics;
pub mod user_mode;
pub mod keyboard;
pub mod ipc;
pub mod display;
pub mod scheduler;

pub fn test_panic_handler(info: &PanicInfo) -> ! {
    log::error!("[failed]");
    log::error!("Error: {}\n", info);
    exit_qemu(QemuExitCode::Failed);
    hlt_loop();
}

// Custom test harness
pub trait KernelTest {
    fn name(&self) -> &'static str;
    fn run(&self) -> TestResult;
}

impl<F> KernelTest for F
where
    F: Fn() -> TestResult,
{
    fn name(&self) -> &'static str {
        core::any::type_name::<F>()
    }

    fn run(&self) -> TestResult {
        self()
    }
}

#[derive(Debug)]
pub enum TestResult {
    Ok,
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestGroup {
    Memory,     // physical_memory, vaddr_allocator, mmap
    Time,       // time
    Interrupts, // interrupts, timer_interrupt
    Graphics,   // graphics
    UserMode,   // user_mode (diagnostic + scheduler handoff)
    Keyboard,   // keyboard
    Ipc,        // ipc
    Display,    // display_owner, get_module
    Scheduler,  // scheduler, spawn
}

pub struct TestEntry {
    pub group: TestGroup,
    pub test: &'static dyn KernelTest,
}

pub fn parse_test_group(cmdline: &[u8]) -> Option<TestGroup> {
    let s = core::str::from_utf8(cmdline).ok()?;
    let prefix = "test_suite=";
    let pos = s.find(prefix)?;
    let value = s[pos + prefix.len()..].split_whitespace().next()?;
    match value {
        "mem"        => Some(TestGroup::Memory),
        "time"       => Some(TestGroup::Time),
        "interrupts" => Some(TestGroup::Interrupts),
        "graphics"   => Some(TestGroup::Graphics),
        "usermode"   => Some(TestGroup::UserMode),
        "keyboard"   => Some(TestGroup::Keyboard),
        "ipc"        => Some(TestGroup::Ipc),
        "display"    => Some(TestGroup::Display),
        "scheduler"  => Some(TestGroup::Scheduler),
        _            => None,
    }
}

pub fn tests() -> &'static [TestEntry] {
    &[
        // Time
        TestEntry { group: TestGroup::Time, test: &time::tsc_calibration },
        TestEntry { group: TestGroup::Time, test: &time::pit_sleep },

        // Memory — virtual address allocator
        TestEntry { group: TestGroup::Memory, test: &memory::vaddr::allocate_kernel_page },
        TestEntry { group: TestGroup::Memory, test: &memory::vaddr::allocate_user_page },
        TestEntry { group: TestGroup::Memory, test: &memory::vaddr::allocate_multiple_pages },

        // Memory — mmap
        TestEntry { group: TestGroup::Memory, test: &memory::mmap::test_user_vaddr_allocate },
        TestEntry { group: TestGroup::Memory, test: &memory::mmap::test_user_vaddr_free },
        TestEntry { group: TestGroup::Memory, test: &memory::mmap::test_user_vaddr_no_overlap },
        TestEntry { group: TestGroup::Memory, test: &memory::mmap::test_mmap_flags_in_api },

        // Memory — physical frames
        TestEntry { group: TestGroup::Memory, test: &memory::physical::alloc_one_frame },
        TestEntry { group: TestGroup::Memory, test: &memory::physical::free_and_reuse_kernel_frame },
        TestEntry { group: TestGroup::Memory, test: &memory::physical::frame_alignment },
        TestEntry { group: TestGroup::Memory, test: &memory::physical::kernel_type },
        TestEntry { group: TestGroup::Memory, test: &memory::physical::user_type },
        TestEntry { group: TestGroup::Memory, test: &memory::physical::exhaustion },
        TestEntry { group: TestGroup::Memory, test: &memory::physical::duplicate_allocation },

        // Interrupts
        TestEntry { group: TestGroup::Interrupts, test: &interrupts::gdt_loaded },
        TestEntry { group: TestGroup::Interrupts, test: &interrupts::idt_loaded },
        TestEntry { group: TestGroup::Interrupts, test: &interrupts::breakpoint_exception },
        TestEntry { group: TestGroup::Interrupts, test: &interrupts::timer::timer_interrupt_fires },

        // Graphics
        TestEntry { group: TestGroup::Graphics, test: &graphics::basic_draw },
        TestEntry { group: TestGroup::Graphics, test: &graphics::bounding_box_valid },

        // User mode (diagnostic, no scheduler handoff)
        TestEntry { group: TestGroup::UserMode, test: &user_mode::test_user_selector_rpl },
        TestEntry { group: TestGroup::UserMode, test: &user_mode::test_lower_half_end_canonical },
        TestEntry { group: TestGroup::UserMode, test: &user_mode::test_user_task_creation },
        TestEntry { group: TestGroup::UserMode, test: &user_mode::test_user_page_table_kernel_mapped },
        TestEntry { group: TestGroup::UserMode, test: &user_mode::test_user_task_iretq_frame },

        // Keyboard
        TestEntry { group: TestGroup::Keyboard, test: &keyboard::test_key_a_press },
        TestEntry { group: TestGroup::Keyboard, test: &keyboard::test_key_release_ignored },
        TestEntry { group: TestGroup::Keyboard, test: &keyboard::test_shift_produces_uppercase },
        TestEntry { group: TestGroup::Keyboard, test: &keyboard::test_enter_key },
        TestEntry { group: TestGroup::Keyboard, test: &keyboard::test_arrow_keys },
        TestEntry { group: TestGroup::Keyboard, test: &keyboard::test_buffer_empty_after_drain },
        TestEntry { group: TestGroup::Keyboard, test: &keyboard::test_multiple_keys_order },
        TestEntry { group: TestGroup::Keyboard, test: &keyboard::test_capslock_toggle },

        // IPC
        TestEntry { group: TestGroup::Ipc, test: &ipc::test_channel_create },
        TestEntry { group: TestGroup::Ipc, test: &ipc::test_send_recv },
        TestEntry { group: TestGroup::Ipc, test: &ipc::test_send_on_recv_fails },
        TestEntry { group: TestGroup::Ipc, test: &ipc::test_recv_on_send_fails },
        TestEntry { group: TestGroup::Ipc, test: &ipc::test_close_then_fail },
        TestEntry { group: TestGroup::Ipc, test: &ipc::test_channel_full },
        TestEntry { group: TestGroup::Ipc, test: &ipc::test_recv_closed_then_send_fails },
        TestEntry { group: TestGroup::Ipc, test: &ipc::test_fifo_order },

        // Display
        TestEntry { group: TestGroup::Display, test: &display::owner::test_no_current_task_is_not_owner },
        TestEntry { group: TestGroup::Display, test: &display::owner::test_no_owner_is_not_owner },
        TestEntry { group: TestGroup::Display, test: &display::owner::test_non_owner_get_bounding_box_rejected },
        TestEntry { group: TestGroup::Display, test: &display::owner::test_transfer_display_not_owner },
        TestEntry { group: TestGroup::Display, test: &display::owner::test_transfer_display_no_current_task },
        TestEntry { group: TestGroup::Display, test: &display::owner::test_display_owner_atomic },
        TestEntry { group: TestGroup::Display, test: &display::modules::test_init_task_module_exists },
        TestEntry { group: TestGroup::Display, test: &display::modules::test_display_server_module_exists },
        TestEntry { group: TestGroup::Display, test: &display::modules::test_nonexistent_module_missing },
        TestEntry { group: TestGroup::Display, test: &display::modules::test_module_has_nonzero_size },

        // Scheduler
        TestEntry { group: TestGroup::Scheduler, test: &scheduler::spawn::test_spawn_error_invalid_elf },
        TestEntry { group: TestGroup::Scheduler, test: &scheduler::spawn::test_spawn_creates_task },
        TestEntry { group: TestGroup::Scheduler, test: &scheduler::spawn::test_spawn_child_arg },
        TestEntry { group: TestGroup::Scheduler, test: &scheduler::simple_task_creation },
        TestEntry { group: TestGroup::Scheduler, test: &scheduler::stack::test_context_switch_registers },
        TestEntry { group: TestGroup::Scheduler, test: &scheduler::stack::test_stack_alignment },
        TestEntry { group: TestGroup::Scheduler, test: &scheduler::stack::test_timer_stack_alignment },

        // Scheduler handoff — enables interrupts and never returns.
        // MUST remain last in the list.
        TestEntry { group: TestGroup::UserMode, test: &user_mode::test_user_task_runs },
    ]
}

pub fn run_tests(filter: Option<TestGroup>) -> ! {
    let all_tests = tests();

    // When a group filter is active, skip the scheduler-handoff test unless it
    // belongs to the requested group (it enables interrupts and never returns).
    let filtered: Vec<&TestEntry> = all_tests
        .iter()
        .filter(|e| filter.map_or(true, |g| e.group == g))
        .collect();

    if let Some(group) = filter {
        log::info!("Running {:?} tests ({} total)", group, filtered.len());
    } else {
        log::info!("Running all {} kernel tests", filtered.len());
    }

    let mut failed = 0;
    for entry in &filtered {
        let result = entry.test.run();
        match result {
            TestResult::Ok => log::info!("{} [ok]", entry.test.name()),
            TestResult::Failed(msg) => {
                log::error!("{} [failed] - {}", entry.test.name(), msg);
                failed += 1;
            }
        }
    }

    if failed == 0 {
        log::info!("All tests passed!");
        exit_qemu(QemuExitCode::Success);
    } else {
        log::error!("{} test(s) failed", failed);
        exit_qemu(QemuExitCode::Failed);
    }

    hlt_loop();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed  = 0x11,
}

pub fn exit_qemu(exit_code: QemuExitCode) {
    use x86_64::instructions::port::Port;

    unsafe {
        let mut port = Port::new(0xf4);
        port.write(exit_code as u32);
    }
}

