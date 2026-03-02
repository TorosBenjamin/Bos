#![cfg_attr(not(target_os = "linux"), no_std)]
#![cfg_attr(not(target_os = "linux"), feature(alloc_error_handler))]

#[cfg(not(target_os = "linux"))]
extern crate alloc;

// Re-export the `egui` namespace so app code writes `use bos_egui::egui`.
// On Linux this is the real egui crate; on Bos it is a minimal software stub.
#[cfg(target_os = "linux")]
pub use ::egui;

#[cfg(not(target_os = "linux"))]
pub mod egui {
    pub use crate::bos::stub_egui::*;
}

/// Apps implement this trait.  The `egui::Context` the closure receives is the
/// real egui context on Linux and our software stub on Bos.
pub trait App {
    fn update(&mut self, ctx: &egui::Context);
}

// ── Linux ────────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
pub fn run<A: App + 'static>(name: &str, app: A) {
    struct Wrapper<A: App>(A);
    impl<A: App> eframe::App for Wrapper<A> {
        fn update(&mut self, ctx: &::egui::Context, _frame: &mut eframe::Frame) {
            self.0.update(ctx);
        }
    }
    eframe::run_native(
        name,
        Default::default(),
        Box::new(|_cc| Ok(Box::new(Wrapper(app)))),
    )
    .unwrap();
}

// ── Bos ──────────────────────────────────────────────────────────────────────

#[cfg(not(target_os = "linux"))]
pub fn run<A: App + 'static>(name: &str, app: A) -> ! {
    bos::run(name, app)
}

#[cfg(not(target_os = "linux"))]
mod bos;
