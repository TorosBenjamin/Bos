//! # bos_egui — Egui Platform Abstraction for Bos OS
//!
//! Provides a unified GUI framework that works on both Linux (real egui + eframe)
//! and Bos (a minimal software-rendered stub with the same API surface).
//!
//! ## Architecture
//!
//! On **Linux**, `bos_egui::egui` re-exports the real `egui` crate, and [`run`]
//! launches an eframe native window. This allows developing and testing GUI apps
//! on the host before deploying to Bos.
//!
//! On **Bos** (`x86_64-unknown-none`), `bos_egui::egui` is a software stub that
//! provides `Context`, `CentralPanel`, `Ui`, `Response`, and basic widgets
//! (buttons, labels, text inputs, selectable labels). Rendering uses
//! `embedded_graphics` with fixed `FONT_8X13` / `FONT_8X13_BOLD` fonts into the
//! window's raw pixel buffer.
//!
//! ## Usage
//!
//! ```ignore
//! use bos_egui::{egui, App};
//!
//! struct MyApp;
//!
//! impl App for MyApp {
//!     fn update(&mut self, ctx: &egui::Context) {
//!         egui::CentralPanel::default().show(ctx, |ui| {
//!             ui.heading("Hello");
//!             if ui.button("Click me").clicked() { /* ... */ }
//!         });
//!     }
//! }
//! ```
//!
//! ## Available Widgets (Bos stub)
//!
//! - `ui.heading(text)` / `ui.label(text)` — text with `FONT_8X13_BOLD` / `FONT_8X13`
//! - `ui.button(text)` — clickable button with hover highlight
//! - `ui.text_edit_singleline(text, focused, cursor_visible)` — editable text field
//! - `ui.selectable_label(selected, text)` — row with selection highlight
//! - `ui.separator()` — horizontal line
//! - `ui.horizontal(|ui| { ... })` — lay out widgets side by side
//! - `ui.canvas()` — raw pixel drawing area for custom rendering

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
    fn child_update(&mut self, _ctx: &egui::Context) {}
}

/// Ask the run loop to render another frame even if no input event arrived.
///
/// Call this from within `App::update` when you change state that requires a
/// follow-up render (e.g. transitioning from "Idle" to "WillFetch").
#[cfg(not(target_os = "linux"))]
pub fn request_redraw() { bos::request_redraw_impl(); }
#[cfg(target_os = "linux")]
pub fn request_redraw() {}

/// Ask the run loop to render another frame after at most `ms` milliseconds.
///
/// The run loop will sleep on the window's event channel with this timeout,
/// so it wakes on either user input or the timer — whichever comes first.
/// Use this for cursor blink, animations, or periodic polling.
#[cfg(not(target_os = "linux"))]
pub fn request_timed_redraw(ms: u32) { bos::request_timed_redraw_impl(ms); }
#[cfg(target_os = "linux")]
pub fn request_timed_redraw(_ms: u32) {}

/// Request a new floating OS window to be opened.
///
/// On Bos the next run-loop iteration will create a compositor-managed floating window
/// and call `App::child_update` each frame for its contents.
/// On Linux this is a no-op (child windows are not supported in the eframe backend).
#[cfg(not(target_os = "linux"))]
pub fn open_child_window(w: u32, h: u32) { bos::request_open_child(w, h); }

#[cfg(target_os = "linux")]
pub fn open_child_window(_w: u32, _h: u32) {
    
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
