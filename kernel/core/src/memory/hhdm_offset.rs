use crate::limine_requests::HHDM_REQUEST;
use core::fmt::Debug;
use limine::response::HhdmResponse;
use x86_64::VirtAddr;

/// Wrapper around u64 representing HHDM offset
#[derive(Clone, Copy)]
pub struct HhdmOffset(u64);

impl Debug for HhdmOffset {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "HhdmOffset(0x{:X})", self.0)
    }
}

impl From<&'static HhdmResponse> for HhdmOffset {
    fn from(value: &'static HhdmResponse) -> Self {
        Self(value.offset())
    }
}

impl From<HhdmOffset> for u64 {
    fn from(value: HhdmOffset) -> Self {
        value.0
    }
}

impl From<HhdmOffset> for VirtAddr {
    fn from(value: HhdmOffset) -> Self {
        VirtAddr::new(value.0)
    }
}

impl HhdmOffset {
    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

pub fn hhdm_offset() -> HhdmOffset {
    HHDM_REQUEST.get_response().unwrap().into()
}
