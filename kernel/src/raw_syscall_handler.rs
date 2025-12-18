use core::arch::{asm, naked_asm};
use core::mem::offset_of;
use core::sync::atomic::Ordering;
use x86_64::registers::control::{Efer, EferFlags};
use x86_64::registers::model_specific::LStar;
use x86_64::VirtAddr;
use crate::memory::cpu_local_data::{get_local, CpuLocalData};
use crate::memory::guarded_stack::{GuardedStack, StackId, StackType};
use kernel_api_types::graphics::{GraphicsResult, PixelData, Rect};
use kernel_api_types::SysCallNumber;
use crate::syscall_handlers::{sys_draw_iter, sys_fill_solid, sys_get_bounding_box};

#[unsafe(naked)]
unsafe extern "sysv64" fn raw_syscall_handler() -> ! {
    naked_asm!(
        "
            // Save the user mode stack pointer
            mov gs:[{syscall_handler_scratch_offset}], rsp
            // Switch to the kernel stack pointer
            mov rsp, gs:[{syscall_handler_stack_pointer_offset}]

            // input[9]
            push gs:[{syscall_handler_scratch_offset}]

            // input[8]
            // Save `rcx` before modifying it
            push rcx

            // input[7]
            push r11

            // input[6]
            push rax

            // Convert `syscall`s `r10` input to `sysv64`s `rcx` input
            mov rcx, r10

            call {syscall_handler}
        ",
        syscall_handler_scratch_offset = const offset_of!(CpuLocalData, syscall_handler_scratch),
        syscall_handler_stack_pointer_offset = const offset_of!(CpuLocalData, syscall_handler_stack_pointer),
        syscall_handler = sym syscall_handler,
    )
}

unsafe extern "sysv64" fn syscall_handler(
    input0: u64,
    input1: u64,
    input2: u64,
    input3: u64,
    input4: u64,
    input5: u64,
    input6: u64,
    rflags: u64,
    return_instruction_pointer: u64,
    return_stack_pointer: u64,) -> ! {

    let inputs = [input1, input2, input3, input4, input5, input6];
    let ret = unsafe { dispatch_syscall(input0, &inputs)};

    // Output
    unsafe {
        asm!(
        "
            mov rsp, {}
            sysretq
        ",
        in(reg) return_stack_pointer,

        // Restore the stack
        in("rcx") return_instruction_pointer,
        in("r11") rflags,
        in("rdi") inputs[0],
        in("rsi") inputs[1],
        in("rdx") inputs[2],
        in("r10") inputs[3],
        in("r8") inputs[4],
        in("r9") inputs[5],
        in("rax") ret,
        options(noreturn)
        )
    }
}

type SyscallFn = fn(u64, u64, u64, u64, u64, u64) -> u64;
static mut SYS_CALL_TABLE: [Option<SyscallFn>; 256] = [None; 256];

unsafe fn dispatch_syscall(syscall_number: u64, args: &[u64;6]) -> u64 {
    if let Some(f) = SYS_CALL_TABLE[syscall_number as usize] {
        f(args[0], args[1], args[2], args[3], args[4], args[5])
    } else {
        0xFFFF_FFFF_FFFF_FFFF
    }
}

pub fn init() {
    let local = get_local();
    let syscall_handler_stack = GuardedStack::new_kernel(
        64 * 0x400,
        StackId {
            _type: StackType::SyscallHandler,
            cpu_id: local.kernel_id,
        },

    );
    local
        .syscall_handler_stack_pointer
        .store(syscall_handler_stack.top().as_u64(), Ordering::Relaxed);

    // Enable syscall in IA32_EFER
    unsafe {
        Efer::update(|flags| {
            *flags = flags.union(EferFlags::SYSTEM_CALL_EXTENSIONS);
        })
    };

    // This tells the CPU the address of our syscall handler
    LStar::write(VirtAddr::from_ptr(raw_syscall_handler as *const ()));

    unsafe {
        SYS_CALL_TABLE[SysCallNumber::DrawIter as usize] = Some(sys_draw_iter);
        SYS_CALL_TABLE[SysCallNumber::FillSolid as usize] = Some(sys_fill_solid);
        SYS_CALL_TABLE[SysCallNumber::GetBoundingBox as usize] = Some(sys_get_bounding_box);
    }
}