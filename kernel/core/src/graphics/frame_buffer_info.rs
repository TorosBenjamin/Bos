use crate::graphics::rgb_pixel::RgbPixel;
use x86_64::PhysAddr;

#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct FrameBufferInfo {
    pub width: u64,
    pub height: u64,
    pub pitch: u64,
    pub bits_per_pixel: u16,
    pub pixel: RgbPixel,
    /// Physical address of the framebuffer (for mapping into user space)
    pub phys_addr: PhysAddr,
}

impl From<&limine::framebuffer::Framebuffer<'_>> for FrameBufferInfo {
    fn from(framebuffer: &limine::framebuffer::Framebuffer) -> Self {
        use crate::memory::hhdm_offset::hhdm_offset;

        // Limine gives us a virtual address in HHDM; compute the physical address
        let virt_addr = framebuffer.addr() as u64;
        let hhdm = hhdm_offset().as_u64();
        let phys_addr = PhysAddr::new(virt_addr - hhdm);

        log::info!(
            "Framebuffer: virt={:#x}, hhdm={:#x}, computed_phys={:#x}, size={}x{}",
            virt_addr, hhdm, phys_addr.as_u64(),
            framebuffer.width(), framebuffer.height()
        );

        FrameBufferInfo {
            width: framebuffer.width(),
            height: framebuffer.height(),
            pitch: framebuffer.pitch(),
            bits_per_pixel: framebuffer.bpp(),
            pixel: RgbPixel {
                red_mask_size: framebuffer.red_mask_size(),
                red_mask_shift: framebuffer.red_mask_shift(),
                green_mask_size: framebuffer.green_mask_size(),
                green_mask_shift: framebuffer.green_mask_shift(),
                blue_mask_size: framebuffer.blue_mask_size(),
                blue_mask_shift: framebuffer.blue_mask_shift(),
            },
            phys_addr,
        }
    }
}
