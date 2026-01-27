#[repr(C, packed)]
pub struct Context {
    rflags: usize,
    r15: usize,
    r14: usize,
    r13: usize,
    r12: usize,
    rbp: usize,
    rbx: usize,
    rip: usize,
}

impl Context {
    pub fn new(rip: usize) -> Context {
        Context {
            // Interrupts enabled
            rflags: 1 << 9,
            r15: 0,
            r14: 0,
            r13: 0,
            r12: 0,
            rbp: 0,
            rbx: 0,
            rip,
        }
    }

    /// Store 'value' in first register (r15)
    pub fn set_first_register(&mut self, value: usize) {
        self.r15 = value;
    }
}

/// Assembly
/// Save context registers by pushing them on the stack
#[macro_export]
macro_rules! save_context {
    () => (
        r#"
            push rbx
            push rbp
            push r12
            push r13
            push r14
            push r15
            pushfq
        "#
    )
}

/// Assembly
/// Switch stacks
/// * The 'rdi' register must contain the previous process stack pointer
/// * The 'rsi' register must contain the next process stack pointer
#[macro_export]
macro_rules! switch_stacks {
    () => (
        // switch the stack pointers
        r#"
            mov [rdi], rsp
            mov rsp, rsi
        "#
    );
}

/// Assembly
/// Restore context by popping them of the stack
#[macro_export]
macro_rules! restore_context {
    () => (
        r#"
            popfq
            pop r15
            pop r14
            pop r13
            pop r12
            pop rbp
            pop rbx
        "#
    );
}

/// Switch context between two tasks
/// Safety: Unsafe because it modifies the stack
#[unsafe(naked)]
pub unsafe extern "C" fn switch(prev_stack_pointer: *mut usize, next_stack_pointer_value: usize) {
    // Logs are not allowed here
    core::arch::naked_asm!(
        "push [rsp]", // fake rip for Context struct alignment
        save_context!(),
        switch_stacks!(),
        restore_context!(),
        "add rsp, 8", // pop fake rip
        "ret",
    );
}

/// Switch to a new task
/// Safety: stack must be valid and 'entry' must not return
pub unsafe fn switch_to_new(_stack_pointer: *mut usize ) {
    unsafe {
        core::arch::asm!(
            "mov rsp, [rdi]",
            restore_context!(),
            "ret",
            options(noreturn)
        )
    }
}
