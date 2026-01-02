use crate::memory::MEMORY;
use crate::memory::physical_memory::{KernelMemoryUsageType, MemoryType};
use alloc::collections::BTreeMap;
use core::arch::naked_asm;
use core::num::NonZero;
use ez_paging::{ConfigurableFlags, ManagedL4PageTable, Page, PageSize};
use x86_64::VirtAddr;
use x86_64::registers::model_specific::PatMemoryType;

pub const NORMAL_STACK_SIZE: u64 = 64 * 0x400;
pub const EXCEPTION_HANDLER_STACK_SIZE: u64 = 64 * 0x400;

pub const STACK_PAGE_SIZE: PageSize = PageSize::_4KiB;

// Keep track of stack guard pages for debugging
pub static STACK_GUARD_PAGES: spin::Mutex<BTreeMap<Page, StackInfo>> =
    spin::Mutex::new(BTreeMap::new());

#[derive(Debug, Clone, Copy)]
pub enum StackType {
    Normal,
    ExceptionHandler,
    SyscallHandler,
}

#[derive(Debug, Clone, Copy)]
pub struct StackId {
    pub _type: StackType,
    #[allow(unused)]
    pub cpu_id: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct StackInfo {
    #[allow(unused)]
    id: StackId,
    #[allow(unused)]
    size: u64,
}

#[derive(Debug)]
pub struct GuardedStack {
    top: VirtAddr,
}

impl GuardedStack {
    pub fn allocate_stack(size: u64, id: StackId) -> Self {
        let memory = MEMORY.get().unwrap();
        let mut physical_memory = memory.physical_memory.lock();
        let mut virtual_memory = memory.virtual_memory.lock();

        let n_mapped_pages = size.div_ceil(STACK_PAGE_SIZE.byte_len_u64());
        let n_virtual_pages = n_mapped_pages + 1; // +1 for the guard page

        // allocate contiguous virtual pages including the guard page
        let allocated_pages = virtual_memory
            .allocate_kernel_contiguous_pages(
                STACK_PAGE_SIZE,
                NonZero::new(n_virtual_pages).unwrap(),
            )
            .unwrap();

        // create the guard page
        let guard_page = Page::new(allocated_pages.start_addr(), STACK_PAGE_SIZE).unwrap();
        STACK_GUARD_PAGES
            .lock()
            .insert(guard_page, StackInfo { id, size });

        // start of the real stack memory
        let start_page = guard_page.offset(1).unwrap();

        // map the real stack pages
        for i in 0..n_mapped_pages {
            let page = start_page.offset(i).unwrap();
            let frame = physical_memory
                .allocate_frame_with_type(
                    STACK_PAGE_SIZE,
                    MemoryType::UsedByKernel(KernelMemoryUsageType::Stack),
                )
                .unwrap();
            let flags = ConfigurableFlags {
                writable: true,
                executable: false,
                pat_memory_type: PatMemoryType::WriteBack,
            };
            let mut frame_allocator = physical_memory.get_kernel_frame_allocator();
            unsafe {
                virtual_memory
                    .l4_mut()
                    .map_page(page, frame, flags, &mut frame_allocator)
            }
            .unwrap();
        }
        Self {
            top: (start_page.start_addr() + n_mapped_pages * STACK_PAGE_SIZE.byte_len_u64()),
        }
    }

    pub fn new_kernel(size: u64, id: StackId) -> Self {
        Self::allocate_stack(size, id)
    }

    pub fn new_user(size: u64, id: StackId, _address_space: &mut ManagedL4PageTable) -> Self {
        Self::allocate_stack(size, id)
    }

    pub fn top(&self) -> VirtAddr {
        self.top
    }

    pub fn switch(self, f: extern "sysv64" fn() -> !) {
        let new_rsp = self.top.as_u64();
        unsafe { switch_to(new_rsp, f) }
    }
}

#[unsafe(naked)]
unsafe extern "sysv64" fn switch_to(new_rsp: u64, f: extern "sysv64" fn() -> !) {
    naked_asm!(
        "
        mov rsp, rdi
        call rsi
        "
    );
}
