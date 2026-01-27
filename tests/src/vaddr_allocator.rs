use kernel::memory::MEMORY;
use crate::TestResult;
use ez_paging::PageSize;
use core::num::NonZero;
use alloc::string::String;

pub fn allocate_kernel_page() -> TestResult {
    let mut vm = MEMORY.get().unwrap().virtual_memory.lock();
    let page = vm.allocate_kernel_contiguous_pages(PageSize::_4KiB, NonZero::new(1).unwrap());
    
    if page.is_some() {
        TestResult::Ok
    } else {
        TestResult::Failed(String::from("Failed to allocate kernel page"))
    }
}

pub fn allocate_user_page() -> TestResult {
    let mut vm = MEMORY.get().unwrap().virtual_memory.lock();
    let page = vm.allocate_user_contiguous_pages(PageSize::_4KiB, NonZero::new(1).unwrap());
    
    if page.is_some() {
        TestResult::Ok
    } else {
        TestResult::Failed(String::from("Failed to allocate user page"))
    }
}

pub fn allocate_multiple_pages() -> TestResult {
    let mut vm = MEMORY.get().unwrap().virtual_memory.lock();
    let n_pages = 5;
    let page = vm.allocate_kernel_contiguous_pages(PageSize::_4KiB, NonZero::new(n_pages).unwrap());
    
    if page.is_some() {
        TestResult::Ok
    } else {
        TestResult::Failed(alloc::format!("Failed to allocate {} contiguous kernel pages", n_pages))
    }
}
