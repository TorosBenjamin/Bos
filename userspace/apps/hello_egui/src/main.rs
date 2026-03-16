#![cfg_attr(not(target_os = "linux"), no_std)]
#![cfg_attr(not(target_os = "linux"), no_main)]

#[cfg(not(target_os = "linux"))]
extern crate alloc;
#[cfg(not(target_os = "linux"))]
use alloc::format;

use bos_egui::{App, egui};

#[derive(Default)]
struct HelloApp {
    counter: i32,
    child_open: bool,
}

impl App for HelloApp {
    fn update(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Hello from Bos!");
            ui.label("A cross-platform egui app running on a custom OS.");
            ui.separator();
            if ui.button("Increment").clicked() {
                self.counter += 1;
            }
            ui.label(format!("Count: {}", self.counter));
            ui.separator();
            if !self.child_open {
                if ui.button("Open Window").clicked() {
                    self.child_open = true;
                    bos_egui::open_child_window(400, 300);
                }
            } else {
                ui.label("(floating window is open)");
            }
        });
    }

    fn child_update(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Floating Window");
            ui.label("Opened from hello_egui via bos_egui::open_child_window.");
        });
    }
}

#[cfg(not(target_os = "linux"))]
#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point(_arg: u64) -> ! {
    bos_egui::run("Hello Egui", HelloApp::default())
}

#[cfg(target_os = "linux")]
fn main() {
    bos_egui::run("Hello Egui", HelloApp::default());
}

#[cfg(not(target_os = "linux"))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {
        ulib::sys_yield();
    }
}
