#![cfg_attr(not(target_os = "linux"), no_std)]
#![cfg_attr(not(target_os = "linux"), no_main)]

#[cfg(not(target_os = "linux"))]
extern crate alloc;
#[cfg(not(target_os = "linux"))]
use alloc::format;
#[cfg(not(target_os = "linux"))]
use alloc::string::String;
#[cfg(not(target_os = "linux"))]
use alloc::string::ToString;

use bos_egui::{App, egui};

struct HelloApp {
    counter: i32,
}

impl App for HelloApp {
    fn update(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Hello from Bos!");
            ui.label("A cross-platform egui app running on a custom OS.");
            ui.separator();
            let mut text: String = "Hello".to_string();
            ui.text_edit_multiline(&mut text);
            if ui.button("Increment").clicked() {
                self.counter += 1;
            }
            ui.label(format!("Count: {}", self.counter));
        });
    }
}

#[cfg(not(target_os = "linux"))]
#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point(_arg: u64) -> ! {
    bos_egui::run("Hello Egui", HelloApp { counter: 0 })
}

#[cfg(target_os = "linux")]
fn main() {
    bos_egui::run("Hello Egui", HelloApp { counter: 0 });
}

#[cfg(not(target_os = "linux"))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
