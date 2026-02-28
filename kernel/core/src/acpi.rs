use crate::memory::MEMORY;
use acpi::aml::AmlError;
use acpi::{AcpiTables, Handle, PciAddress, PhysicalMapping};
use core::marker::PhantomData;
use core::num::NonZero;
use core::ptr::NonNull;
use limine::response::RsdpResponse;
use x86_64::{PhysAddr, VirtAddr};
use x86_64::structures::paging::{Mapper, Page, PageSize, PageTableFlags, PhysFrame, Size4KiB};

#[derive(Debug, Clone)]
struct KernelAcpiHandler {
    phantom: PhantomData<NonNull<()>>,
}

impl acpi::Handler for KernelAcpiHandler {
    unsafe fn map_physical_region<T>(
        &self,
        physical_address: usize,
        size: usize,
    ) -> PhysicalMapping<Self, T> {
        let page_size = Size4KiB::SIZE; // 4096

        let memory = MEMORY.get().unwrap();
        let mut virtual_memory = memory.virtual_memory.lock();
        let mut physical_memory = memory.physical_memory.lock();

        // Calculate the page-aligned start and end
        let phys_start = physical_address as u64;
        let phys_end = phys_start + size as u64;

        let aligned_phys_start = phys_start / page_size * page_size;
        let aligned_phys_end = (phys_end + page_size - 1) / page_size * page_size;
        let n_pages = (aligned_phys_end - aligned_phys_start) / page_size;

        // 1. Allocate virtual pages
        let start_page = virtual_memory
            .allocate_kernel_contiguous_pages(
                NonZero::new(n_pages).expect("ACPI mapping must be at least 1 byte"),
            )
            .expect("Failed to allocate virtual memory for ACPI");

        // 2. Prepare mapping
        let start_frame: PhysFrame<Size4KiB> = PhysFrame::containing_address(PhysAddr::new(aligned_phys_start));
        let mut mapper = unsafe { virtual_memory.mapper() };
        let mut frame_allocator = physical_memory.get_kernel_frame_allocator();

        let flags = PageTableFlags::PRESENT; // ACPI tables are usually Read-Only

        for i in 0..n_pages {
            let page = start_page + i;
            let frame = start_frame + i;

            unsafe {
                mapper.map_to(page, frame, flags, &mut frame_allocator)
                    .expect("Failed to map ACPI page")
                    .flush();
            }
        }

        // Calculate the virtual start address (adding back the offset from alignment)
        let offset_in_page = phys_start % page_size;
        let virt_start = start_page.start_address() + offset_in_page;

        PhysicalMapping {
            physical_start: physical_address,
            virtual_start: NonNull::new(virt_start.as_mut_ptr()).unwrap(),
            region_length: size,
            mapped_length: (n_pages * page_size) as usize,
            handler: self.clone(),
        }
    }

    fn unmap_physical_region<T>(region: &PhysicalMapping<Self, T>) {
        let memory = MEMORY.get().unwrap();
        let mut virtual_memory = memory.virtual_memory.lock();
        let mut mapper = unsafe { virtual_memory.mapper() };

        let page_size = Size4KiB::SIZE;
        let virt_addr = VirtAddr::from_ptr(region.virtual_start.as_ptr());
        let aligned_virt_start = virt_addr.align_down(page_size);

        let start_page: Page<Size4KiB> = Page::containing_address(aligned_virt_start);
        let n_pages = (region.mapped_length as u64) / page_size;

        for i in 0..n_pages {
            let page = start_page + i;
            // Unmap the page. We don't free the physical frame because
            // ACPI tables are in reserved/mapped bootloader memory.
            if let Ok((_, _, flush)) = mapper.unmap(page) {
                flush.flush();
            }
        }

        // TODO: Free up vaddr
    }

    fn read_u8(&self, _address: usize) -> u8 {
        todo!()
    }

    fn read_u16(&self, _address: usize) -> u16 {
        todo!()
    }

    fn read_u32(&self, _address: usize) -> u32 {
        todo!()
    }

    fn read_u64(&self, _address: usize) -> u64 {
        todo!()
    }

    fn write_u8(&self, _address: usize, _value: u8) {
        todo!()
    }

    fn write_u16(&self, _address: usize, _value: u16) {
        todo!()
    }

    fn write_u32(&self, _address: usize, _value: u32) {
        todo!()
    }

    fn write_u64(&self, _address: usize, _value: u64) {
        todo!()
    }

    fn read_io_u8(&self, _port: u16) -> u8 {
        todo!()
    }

    fn read_io_u16(&self, _port: u16) -> u16 {
        todo!()
    }

    fn read_io_u32(&self, _port: u16) -> u32 {
        todo!()
    }

    fn write_io_u8(&self, _port: u16, _value: u8) {
        todo!()
    }

    fn write_io_u16(&self, _port: u16, _value: u16) {
        todo!()
    }

    fn write_io_u32(&self, _port: u16, _value: u32) {
        todo!()
    }

    fn read_pci_u8(&self, _address: PciAddress, _offset: u16) -> u8 {
        todo!()
    }

    fn read_pci_u16(&self, _address: PciAddress, _offset: u16) -> u16 {
        todo!()
    }

    fn read_pci_u32(&self, _address: PciAddress, _offset: u16) -> u32 {
        todo!()
    }

    fn write_pci_u8(&self, _address: PciAddress, _offset: u16, _value: u8) {
        todo!()
    }

    fn write_pci_u16(&self, _address: PciAddress, _offset: u16, _value: u16) {
        todo!()
    }

    fn write_pci_u32(&self, _address: PciAddress, _offset: u16, _value: u32) {
        todo!()
    }

    fn nanos_since_boot(&self) -> u64 {
        todo!()
    }

    fn stall(&self, _microseconds: u64) {
        todo!()
    }

    fn sleep(&self, _milliseconds: u64) {
        todo!()
    }

    fn create_mutex(&self) -> Handle {
        todo!()
    }

    fn acquire(&self, _mutex: Handle, _timeout: u16) -> Result<(), AmlError> {
        todo!()
    }

    fn release(&self, _mutex: Handle) {
        todo!()
    }
}

pub fn parse(rsdp: &RsdpResponse) -> AcpiTables<impl acpi::Handler> {
    let address = rsdp.address();
    unsafe {
        AcpiTables::from_rsdp(
            KernelAcpiHandler {
                phantom: PhantomData,
            },
            address,
        )
    }
    .unwrap()
}
