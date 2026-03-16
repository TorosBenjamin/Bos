use crate::drivers::pci;
use crate::memory::MEMORY;
use crate::memory::cpu_local_data::get_local;
use crate::memory::hhdm_offset::hhdm_offset;
use crate::task::task::{TaskKind, VmaBacking, VmaEntry};
use crate::memory::user_vaddr;
use x86_64::structures::paging::{
    Mapper, OffsetPageTable, Page, PageSize, PageTable, PageTableFlags, PhysFrame, Size4KiB,
};
use x86_64::{PhysAddr, VirtAddr};

/// Syscall: read from PCI configuration space.
///
/// args: bus, device (0..=31), function (0..=7), offset (0..=255), width (1/2/4)
/// returns: read value (zero-extended) on success, u64::MAX on error
pub fn sys_pci_config_read(bus: u64, device: u64, function: u64, offset: u64, width: u64, _: u64) -> u64 {
    if bus > 255 || device > 31 || function > 7 || offset > 255 || !matches!(width, 1 | 2 | 4) {
        return u64::MAX;
    }

    match pci::config_read(bus as u8, device as u8, function as u8, offset as u8, width as u8) {
        Some(val) => val as u64,
        None => u64::MAX,
    }
}

/// Syscall: write to PCI configuration space.
///
/// args: bus, device (0..=31), function (0..=7), offset (0..=255), width (1/2/4), value
/// returns: 0 on success, u64::MAX on error
pub fn sys_pci_config_write(bus: u64, device: u64, function: u64, offset: u64, width: u64, value: u64) -> u64 {
    if bus > 255 || device > 31 || function > 7 || offset > 255 || !matches!(width, 1 | 2 | 4) {
        return u64::MAX;
    }

    if pci::config_write(bus as u8, device as u8, function as u8, offset as u8, width as u8, value as u32) {
        0
    } else {
        u64::MAX
    }
}

/// Syscall: map a PCI device's MMIO BAR into the calling user task's address space.
///
/// args: bus, device (0..=31), function (0..=7), bar_index (0..=5)
/// returns: virtual address of the mapped region on success, 0 on failure
pub fn sys_map_pci_bar(bus: u64, device: u64, function: u64, bar_index: u64, _: u64, _: u64) -> u64 {
    if bus > 255 || device > 31 || function > 7 || bar_index > 5 {
        return 0;
    }

    let (phys_base, size) = match pci::read_bar(bus as u8, device as u8, function as u8, bar_index as u8) {
        Some(v) => v,
        None => return 0,
    };

    let cpu = get_local();
    let task = {
        let rq = cpu.run_queue.get().unwrap().lock();
        match &rq.current_task {
            Some(t) if t.kind == TaskKind::User => t.clone(),
            _ => return 0,
        }
    };

    let n_pages = size.div_ceil(Size4KiB::SIZE);
    let mmio_flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::USER_ACCESSIBLE
        | PageTableFlags::NO_CACHE
        | PageTableFlags::WRITE_THROUGH
        | PageTableFlags::NO_EXECUTE;

    let mut inner = task.inner.lock();

    let vaddr = match user_vaddr::allocate_user_vma(
        &mut inner.user_vmas,
        n_pages,
        VmaEntry { flags: mmio_flags, backing: VmaBacking::EagerlyMapped },
    ) {
        Some(a) => a,
        None => return 0,
    };

    let hhdm_off = hhdm_offset();
    let user_l4_frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(task.cr3));
    let l4_virt = VirtAddr::new(hhdm_off.as_u64() + user_l4_frame.start_address().as_u64());
    let l4_table = unsafe { &mut *l4_virt.as_mut_ptr::<PageTable>() };
    let mut mapper = unsafe { OffsetPageTable::new(l4_table, VirtAddr::new(hhdm_off.as_u64())) };

    let memory = MEMORY.get().unwrap();
    let mut phys_mem = memory.physical_memory.lock();
    let mut frame_allocator = phys_mem.get_user_mode_frame_allocator();

    for i in 0..n_pages {
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(vaddr + i * Size4KiB::SIZE));
        let frame = PhysFrame::<Size4KiB>::containing_address(phys_base + i * Size4KiB::SIZE);

        match unsafe { mapper.map_to(page, frame, mmio_flags, &mut frame_allocator) } {
            Ok(flush) => flush.ignore(),
            Err(_) => {
                drop(frame_allocator);
                // Unmap already-installed pages (MMIO frames themselves are never freed)
                for j in 0..i {
                    let p = Page::<Size4KiB>::containing_address(VirtAddr::new(vaddr + j * Size4KiB::SIZE));
                    if let Ok((_, _, f)) = mapper.unmap(p) {
                        f.ignore();
                    }
                }
                let _ = user_vaddr::free_user_vma(&mut inner.user_vmas, vaddr, n_pages * Size4KiB::SIZE);
                return 0;
            }
        }
    }

    vaddr
}
