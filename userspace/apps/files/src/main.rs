#![cfg_attr(not(target_os = "linux"), no_std)]
#![cfg_attr(not(target_os = "linux"), no_main)]

#[cfg(not(target_os = "linux"))]
extern crate alloc;
use bos_egui::{egui, App};

#[derive(Default)]
pub struct FilesApp {

}

impl App for FilesApp {
    fn update(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Files");
        });
    }
}

#[cfg(not(target_os = "linux"))]
#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point(_arg: u64) -> ! {
    bos_egui::run("Files", FilesApp::default())
}

#[cfg(target_os = "linux")]
fn main() {
    bos_egui::run("Files", FilesApp::default());
}

#[cfg(not(target_os = "linux"))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {
        ulib::sys_yield();
    }
}
