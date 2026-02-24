use crate::memory::MEMORY;
use crate::memory::physical_memory::{KernelMemoryUsageType, MemoryType};
use alloc::collections::BTreeMap;
use core::arch::naked_asm;
use core::num::NonZero;
use x86_64::VirtAddr;
use x86_64::structures::paging::{Mapper, Page, PageSize, PageTableFlags, Size4KiB};

pub const NORMAL_STACK_SIZE: u64 = 256 * 0x400;
pub const EXCEPTION_HANDLER_STACK_SIZE: u64 = 64 * 0x400;

// Keep track of stack guard pages for debugging
pub static STACK_GUARD_PAGES: spin::Mutex<BTreeMap<Page<Size4KiB>, StackInfo>> =
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
    guard_page: Page<Size4KiB>,
    /// Total virtual pages allocated: 1 guard + n_mapped
    n_virtual_pages: u64,
}

impl GuardedStack {
    pub fn allocate_stack(size: u64, id: StackId) -> Self {
        let memory = MEMORY.get().unwrap();
        let mut physical_memory = memory.physical_memory.lock();
        let mut virtual_memory = memory.virtual_memory.lock();

        let page_size = Size4KiB::SIZE; // 4096
        let n_mapped_pages = size.div_ceil(page_size);
        let n_virtual_pages = n_mapped_pages + 1; // +1 for the guard page

        // allocate contiguous virtual pages including the guard page
        let allocated_pages = virtual_memory
            .allocate_kernel_contiguous_pages(
                NonZero::new(n_virtual_pages).expect("Stack size cannot be 0"),
            )
            .expect("Failed to allocate virtual memory for stack");

        // Identify the guard page (the first page in the allocated range)
        let guard_page = allocated_pages;
        STACK_GUARD_PAGES
            .lock()
            .insert(guard_page, StackInfo { id, size });

        // Identify the start of real stack memory (immediately after guard page)
        let start_page = guard_page + 1;

        // Get the mapper and map the real stack pages
        let mut mapper = unsafe { virtual_memory.mapper() };
        let mut frame_allocator = physical_memory.get_kernel_frame_allocator();

        for i in 0..n_mapped_pages {
            let page = start_page + i;
            let frame = frame_allocator
                .allocate_frame_4kib()
                .expect("Failed to allocate physical frame for stack");

            let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;

            unsafe {
                mapper.map_to(page, frame, flags, &mut frame_allocator)
                    .expect("Failed to map stack page")
                    .flush(); // Ensure TLB is updated
            }
        }
        Self {
            // Stack grows DOWN, so top is the end of the last mapped page
            top: start_page.start_address() + (n_mapped_pages * page_size),
            guard_page,
            n_virtual_pages,
        }
    }

    pub fn new_kernel(size: u64, id: StackId) -> Self {
        Self::allocate_stack(size, id)
    }

    pub fn new_user(size: u64, id: StackId) -> Self {
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

impl Drop for GuardedStack {
    fn drop(&mut self) {
        let memory = MEMORY.get().unwrap();
        let mut physical_memory = memory.physical_memory.lock();
        let mut virtual_memory = memory.virtual_memory.lock();
        let mut mapper = unsafe { virtual_memory.mapper() };

        // Unmap and free each mapped page (index 0 is the guard page, skip it)
        for i in 1..self.n_virtual_pages {
            let page = self.guard_page + i;
            if let Ok((frame, _, flush)) =  mapper.unmap(page) {
                flush.flush();
                let _ = physical_memory.free_frame(
                    frame,
                    MemoryType::UsedByKernel(KernelMemoryUsageType::PageTables),
                );
            }
        }

        // Release the virtual address range (guard page + all mapped pages)
        virtual_memory.free_kernel_pages(
            self.guard_page,
            NonZero::new(self.n_virtual_pages).unwrap(),
        );

        // Remove the guard page tracking entry
        STACK_GUARD_PAGES.lock().remove(&self.guard_page);
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
