use crate::memory::cpu_local_data::{CpuLocalData, IN_SYSCALL_HANDLER_OFFSET, CURRENT_CONTEXT_PTR_OFFSET, CURRENT_TASK_KERNEL_STACK_TOP_OFFSET, get_local};
use crate::syscall_handlers::{sys_channel_close, sys_channel_create, sys_channel_recv, sys_channel_send, sys_debug_log, sys_exit, sys_get_bounding_box, sys_get_display_info, sys_get_module, sys_lookup_service, sys_mmap, sys_munmap, sys_read_key, sys_read_mouse, sys_register_service, sys_shutdown, sys_spawn, sys_transfer_display, sys_waitpid, sys_yield};
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
            // SYSCALL always comes from ring 3 — swap GS to get kernel CpuLocalData
            swapgs

            // Save the user mode stack pointer
            mov gs:[{scratch_offset}], rsp
            // Switch to the current task's kernel stack (same stack TSS.RSP0 points to)
            mov rsp, gs:[{kernel_stack_top_offset}]

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
        kernel_stack_top_offset = const CURRENT_TASK_KERNEL_STACK_TOP_OFFSET,
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
    // input0 = rdi = syscall number; input1 = rsi = exit code
    if input0 == SysCallNumber::Exit as u64 {
        sys_exit(input1); // -> !, never returns
    }

    let inputs = [input1, input2, input3, input4, input5, input6];
    let ret = dispatch_syscall(input0, &inputs);

    // Clear in_syscall flag before returning to user mode
    get_local().in_syscall_handler.store(0, Ordering::Relaxed);

    // Output
    unsafe {
        asm!(
        "
            mov rsp, {}
            swapgs
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
static SYSCALL_TABLE: spin::Once<[Option<SyscallFn>; 256]> = spin::Once::new();

fn dispatch_syscall(syscall_number: u64, args: &[u64; 6]) -> u64 {
    let table = SYSCALL_TABLE.get().expect("syscall table not initialized");
    if let Some(f) = table[syscall_number as usize] {
        f(args[0], args[1], args[2], args[3], args[4], args[5])
    } else {
        log::error!("SYSCALL: unknown syscall number {}", syscall_number);
        0xFFFF_FFFF_FFFF_FFFF
    }
}

pub fn init() {
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

    // Mask IF during SYSCALL to prevent the timer from firing before we've
    // finished switching to the kernel stack and saving user context.
    SFMask::write(RFlags::INTERRUPT_FLAG);

    SYSCALL_TABLE.call_once(|| {
        let mut table = [None::<SyscallFn>; 256];
        table[SysCallNumber::GetBoundingBox as usize] = Some(sys_get_bounding_box);
        table[SysCallNumber::GetDisplayInfo as usize] = Some(sys_get_display_info);
        table[SysCallNumber::ReadKey as usize] = Some(sys_read_key);
        table[SysCallNumber::Yield as usize] = Some(sys_yield);
        table[SysCallNumber::Spawn as usize] = Some(sys_spawn);
        table[SysCallNumber::Mmap as usize] = Some(sys_mmap);
        table[SysCallNumber::Munmap as usize] = Some(sys_munmap);
        table[SysCallNumber::ChannelCreate as usize] = Some(sys_channel_create);
        table[SysCallNumber::ChannelSend as usize] = Some(sys_channel_send);
        table[SysCallNumber::ChannelRecv as usize] = Some(sys_channel_recv);
        table[SysCallNumber::ChannelClose as usize] = Some(sys_channel_close);
        table[SysCallNumber::TransferDisplay as usize] = Some(sys_transfer_display);
        table[SysCallNumber::GetModule as usize] = Some(sys_get_module);
        table[SysCallNumber::DebugLog as usize] = Some(sys_debug_log);
        table[SysCallNumber::Waitpid as usize] = Some(sys_waitpid);
        table[SysCallNumber::RegisterService as usize] = Some(sys_register_service);
        table[SysCallNumber::LookupService as usize] = Some(sys_lookup_service);
        table[SysCallNumber::ReadMouse as usize] = Some(sys_read_mouse);
        table[SysCallNumber::Shutdown as usize] = Some(sys_shutdown);
        table
    });
}
