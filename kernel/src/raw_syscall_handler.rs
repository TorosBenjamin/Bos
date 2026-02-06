use crate::memory::cpu_local_data::{CpuLocalData, IN_SYSCALL_HANDLER_OFFSET, CURRENT_CONTEXT_PTR_OFFSET, get_local};
use crate::memory::guarded_stack::{GuardedStack, StackId, StackType};
use crate::syscall_handlers::{sys_channel_close, sys_channel_create, sys_channel_recv, sys_channel_send, sys_exit, sys_get_bounding_box, sys_get_display_info, sys_get_module, sys_mmap, sys_munmap, sys_read_key, sys_spawn, sys_transfer_display, sys_yield};
use crate::task::task::{
    CTX_RAX, CTX_RBP, CTX_RBX, CTX_RCX, CTX_RDI, CTX_RDX, CTX_RSI,
    CTX_R8, CTX_R9, CTX_R10, CTX_R11, CTX_R12, CTX_R13, CTX_R14, CTX_R15,
    CTX_RIP, CTX_CS, CTX_RFLAGS, CTX_RSP, CTX_SS,
};
use core::arch::{asm, naked_asm};
use core::mem::offset_of;
use core::sync::atomic::Ordering;
use kernel_api_types::SysCallNumber;
use x86_64::VirtAddr;
use x86_64::registers::control::{Efer, EferFlags};
use x86_64::registers::model_specific::{LStar, SFMask, Star};
use x86_64::registers::rflags::RFlags;

#[unsafe(naked)]
unsafe extern "sysv64" fn raw_syscall_handler() -> ! {
    naked_asm!(
        "
            // Save the user mode stack pointer
            mov gs:[{scratch_offset}], rsp
            // Switch to the kernel stack pointer
            mov rsp, gs:[{stack_ptr_offset}]

            // --- Save user state to CpuContext ---
            // Push rax (syscall number) to free it as scratch
            push rax
            // Load CpuContext pointer
            mov rax, gs:[{ctx_ptr_offset}]
            test rax, rax
            jz 2f

            // Save all 15 GPRs to CpuContext
            mov [rax + {CTX_RBX}], rbx
            mov [rax + {CTX_RCX}], rcx        // rcx = user RIP (SYSCALL sets it)
            mov [rax + {CTX_RDX}], rdx
            mov [rax + {CTX_RSI}], rsi
            mov [rax + {CTX_RDI}], rdi
            mov [rax + {CTX_RBP}], rbp
            mov [rax + {CTX_R8}],  r8
            mov [rax + {CTX_R9}],  r9
            mov [rax + {CTX_R10}], r10
            mov [rax + {CTX_R11}], r11        // r11 = user RFLAGS (SYSCALL sets it)
            mov [rax + {CTX_R12}], r12
            mov [rax + {CTX_R13}], r13
            mov [rax + {CTX_R14}], r14
            mov [rax + {CTX_R15}], r15
            // Save original rax (syscall number) from stack
            mov rbx, [rsp]
            mov [rax + {CTX_RAX}], rbx

            // Build iretq frame in CpuContext
            mov [rax + {CTX_RIP}], rcx        // user RIP
            mov rbx, 0x23
            mov [rax + {CTX_CS}], rbx         // user CS
            mov [rax + {CTX_RFLAGS}], r11     // user RFLAGS
            mov rbx, gs:[{scratch_offset}]    // user RSP (saved earlier)
            mov [rax + {CTX_RSP}], rbx
            mov rbx, 0x1B
            mov [rax + {CTX_SS}], rbx         // user SS

            // Set in_syscall flag
            mov byte ptr gs:[{in_syscall_offset}], 1

            // Restore rbx — we used it as scratch above, but rbx is callee-saved
            // in the SysV ABI. User code expects it preserved across syscall.
            mov rbx, [rax + {CTX_RBX}]

        2:
            pop rax                           // restore rax (syscall number)

            // --- Original push/call sequence ---
            // input[9]
            push gs:[{scratch_offset}]

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
        scratch_offset = const offset_of!(CpuLocalData, syscall_handler_scratch),
        stack_ptr_offset = const offset_of!(CpuLocalData, syscall_handler_stack_pointer),
        ctx_ptr_offset = const CURRENT_CONTEXT_PTR_OFFSET,
        in_syscall_offset = const IN_SYSCALL_HANDLER_OFFSET,
        syscall_handler = sym syscall_handler,
        CTX_RBX = const CTX_RBX,
        CTX_RCX = const CTX_RCX,
        CTX_RDX = const CTX_RDX,
        CTX_RSI = const CTX_RSI,
        CTX_RDI = const CTX_RDI,
        CTX_RBP = const CTX_RBP,
        CTX_R8 = const CTX_R8,
        CTX_R9 = const CTX_R9,
        CTX_R10 = const CTX_R10,
        CTX_R11 = const CTX_R11,
        CTX_R12 = const CTX_R12,
        CTX_R13 = const CTX_R13,
        CTX_R14 = const CTX_R14,
        CTX_R15 = const CTX_R15,
        CTX_RAX = const CTX_RAX,
        CTX_RIP = const CTX_RIP,
        CTX_CS = const CTX_CS,
        CTX_RFLAGS = const CTX_RFLAGS,
        CTX_RSP = const CTX_RSP,
        CTX_SS = const CTX_SS,
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
    return_stack_pointer: u64,
) -> ! {
    // Handle Exit specially since it diverges (never returns to sysretq)
    if input6 == SysCallNumber::Exit as u64 {
        sys_exit(); // -> !, never returns
    }

    let inputs = [input1, input2, input3, input4, input5, input6];
    let ret = unsafe { dispatch_syscall(input0, &inputs) };

    // Clear in_syscall flag before returning to user mode
    get_local().in_syscall_handler.store(0, Ordering::Relaxed);

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

unsafe fn dispatch_syscall(syscall_number: u64, args: &[u64; 6]) -> u64 {
    // Log syscall entry
    let syscall_name = match syscall_number {
        n if n == SysCallNumber::GetBoundingBox as u64 => "GetBoundingBox",
        n if n == SysCallNumber::Exit as u64 => "Exit",
        n if n == SysCallNumber::Spawn as u64 => "Spawn",
        n if n == SysCallNumber::ReadKey as u64 => "ReadKey",
        n if n == SysCallNumber::Yield as u64 => "Yield",
        n if n == SysCallNumber::Mmap as u64 => "Mmap",
        n if n == SysCallNumber::Munmap as u64 => "Munmap",
        n if n == SysCallNumber::ChannelCreate as u64 => "ChannelCreate",
        n if n == SysCallNumber::ChannelSend as u64 => "ChannelSend",
        n if n == SysCallNumber::ChannelRecv as u64 => "ChannelRecv",
        n if n == SysCallNumber::ChannelClose as u64 => "ChannelClose",
        n if n == SysCallNumber::TransferDisplay as u64 => "TransferDisplay",
        n if n == SysCallNumber::GetModule as u64 => "GetModule",
        n if n == SysCallNumber::GetDisplayInfo as u64 => "GetDisplayInfo",
        _ => "Unknown",
    };

    log::info!("SYSCALL: {} (#{}) args=[{:#x}, {:#x}, {:#x}, {:#x}, {:#x}, {:#x}]",
        syscall_name, syscall_number, args[0], args[1], args[2], args[3], args[4], args[5]);

    unsafe {
        if let Some(f) = SYS_CALL_TABLE[syscall_number as usize] {
            let ret = f(args[0], args[1], args[2], args[3], args[4], args[5]);
            log::info!("SYSCALL: {} -> {:#x}", syscall_name, ret);
            ret
        } else {
            log::error!("SYSCALL: Invalid syscall number {}", syscall_number);
            0xFFFF_FFFF_FFFF_FFFF
        }
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

    // STAR MSR: sysret_base=0x10, syscall_base=0x08
    // SYSCALL: CS = 0x08, SS = 0x08+8 = 0x10 (kernel code/data)
    // SYSRET:  SS = (0x10+8)|3 = 0x1B, CS = (0x10+16)|3 = 0x23 (user data/code)
    unsafe { Star::write_raw(0x10, 0x08) };

    // Mask IF during SYSCALL to prevent timer from firing on the per-CPU
    // syscall handler stack (which has no iretq frame and would corrupt state)
    SFMask::write(RFlags::INTERRUPT_FLAG);

    unsafe {
        SYS_CALL_TABLE[SysCallNumber::GetBoundingBox as usize] = Some(sys_get_bounding_box);
        SYS_CALL_TABLE[SysCallNumber::GetDisplayInfo as usize] = Some(sys_get_display_info);
        SYS_CALL_TABLE[SysCallNumber::ReadKey as usize] = Some(sys_read_key);
        SYS_CALL_TABLE[SysCallNumber::Yield as usize] = Some(sys_yield);
        SYS_CALL_TABLE[SysCallNumber::Spawn as usize] = Some(sys_spawn);
        SYS_CALL_TABLE[SysCallNumber::Mmap as usize] = Some(sys_mmap);
        SYS_CALL_TABLE[SysCallNumber::Munmap as usize] = Some(sys_munmap);
        SYS_CALL_TABLE[SysCallNumber::ChannelCreate as usize] = Some(sys_channel_create);
        SYS_CALL_TABLE[SysCallNumber::ChannelSend as usize] = Some(sys_channel_send);
        SYS_CALL_TABLE[SysCallNumber::ChannelRecv as usize] = Some(sys_channel_recv);
        SYS_CALL_TABLE[SysCallNumber::ChannelClose as usize] = Some(sys_channel_close);
        SYS_CALL_TABLE[SysCallNumber::TransferDisplay as usize] = Some(sys_transfer_display);
        SYS_CALL_TABLE[SysCallNumber::GetModule as usize] = Some(sys_get_module);
    }
}
