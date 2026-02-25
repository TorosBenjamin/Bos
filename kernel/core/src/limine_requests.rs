use core::ffi::CStr;
use limine::BaseRevision;
use limine::modules::InternalModule;
use limine::mp::RequestFlags;
use limine::request::{
    FramebufferRequest, HhdmRequest, KernelFileRequest, MemoryMapRequest, ModuleRequest, MpRequest,
    RequestsEndMarker, RequestsStartMarker, RsdpRequest,
};

pub const INIT_TASK_PATH: &CStr = c"/init_task";
pub const DISPLAY_SERVER_PATH: &CStr = c"/display_server";
pub const BOUNCING_CUBE_1_PATH: &CStr = c"/bouncing_cube_1";
pub const BOUNCING_CUBE_2_PATH: &CStr = c"/bouncing_cube_2";

#[used]
#[unsafe(link_section = ".requests")]
pub static BASE_REVISION: BaseRevision = BaseRevision::new();

#[used]
#[unsafe(link_section = ".requests")]
pub static FRAME_BUFFER_REQUEST: FramebufferRequest = FramebufferRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
pub static MP_REQUEST: MpRequest = MpRequest::new().with_flags(RequestFlags::X2APIC);

#[used]
#[unsafe(link_section = ".requests")]
pub static MEMORY_MAP_REQUEST: MemoryMapRequest = MemoryMapRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
pub static HHDM_REQUEST: HhdmRequest = HhdmRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
pub static RSDP_REQUEST: RsdpRequest = RsdpRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
pub static MODULE_REQUEST: ModuleRequest =
    ModuleRequest::new().with_internal_modules(&[
        &InternalModule::new().with_path(INIT_TASK_PATH),
        &InternalModule::new().with_path(DISPLAY_SERVER_PATH),
        &InternalModule::new().with_path(BOUNCING_CUBE_1_PATH),
        &InternalModule::new().with_path(BOUNCING_CUBE_2_PATH),
    ]);

#[used]
#[unsafe(link_section = ".requests")]
pub static KERNEL_FILE_REQUEST: KernelFileRequest = KernelFileRequest::new();

#[used]
#[unsafe(link_section = ".requests_start_marker")]
static _START_MARKER: RequestsStartMarker = RequestsStartMarker::new();
#[used]
#[unsafe(link_section = ".requests_end_marker")]
static _END_MARKER: RequestsEndMarker = RequestsEndMarker::new();
