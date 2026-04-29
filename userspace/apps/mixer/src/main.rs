//! Mixer — audio test app for Bos OS.
//!
//! Connects to the sound server and lets you play a continuous test tone,
//! pick a waveform, tune frequency and volume, and watch a live waveform preview.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::{format, vec::Vec};
use bos_egui::{egui, App};
use kernel_api_types::SVC_ERR_NOT_FOUND;

// ── Constants ─────────────────────────────────────────────────────────────────

const SAMPLE_RATE: u32 = 44100;
const CHANNELS:    u8  = 2;

/// Stereo sample-pairs sent per frame. 4096 frames ≈ 93 ms of audio at 44100 Hz,
/// which keeps the sound server ring well ahead of real-time even at low (~10–20 ms)
/// frame rates, preventing starvation and cursor-dependent dropouts.
const CHUNK_FRAMES: usize = 4096;

const PREVIEW_POINTS: usize = 128;

// ── Colours (theme) ───────────────────────────────────────────────────────────

const GREEN:  egui::Color32 = egui::Color32::from_rgb(80, 200,  80);
const RED:    egui::Color32 = egui::Color32::from_rgb(220, 80,  80);
const BLUE:   egui::Color32 = egui::Color32::from_rgb( 80, 200, 255);
const DIM:    egui::Color32 = egui::Color32::from_rgb( 80, 120, 140);
const DARK:   egui::Color32 = egui::Color32::from_rgb( 20,  20,  30);
const GRID:   egui::Color32 = egui::Color32::from_rgb( 50,  50,  60);

// ── Waveform ──────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Waveform {
    Sine,
    Square,
    Sawtooth,
    Triangle,
}

impl Waveform {
    const ALL: &'static [(Waveform, &'static str)] = &[
        (Waveform::Sine,     "Sine"),
        (Waveform::Square,   "Square"),
        (Waveform::Sawtooth, "Sawtooth"),
        (Waveform::Triangle, "Triangle"),
    ];

    fn sample(self, phase: f32) -> f32 {
        match self {
            Waveform::Sine     => sine_approx(phase * core::f32::consts::TAU),
            Waveform::Square   => if phase < 0.5 { 1.0 } else { -1.0 },
            Waveform::Sawtooth => 2.0 * phase - 1.0,
            Waveform::Triangle => {
                if phase < 0.5 { 4.0 * phase - 1.0 } else { 3.0 - 4.0 * phase }
            }
        }
    }
}

/// Bhaskara I polynomial sine approximation — accurate to ~0.1%, no libm needed.
fn sine_approx(x: f32) -> f32 {
    use core::f32::consts::{PI, TAU};
    let x = x - TAU * floor(x / TAU);
    let x = if x > PI { x - TAU } else { x };
    let ax   = if x < 0.0 { -x } else { x };
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let xpi  = ax * (PI - ax);
    sign * (16.0 * xpi) / (5.0 * PI * PI - 4.0 * xpi)
}

fn floor(x: f32) -> f32 {
    let xi = x as i32;
    if x < xi as f32 { (xi - 1) as f32 } else { xi as f32 }
}

// ── App ───────────────────────────────────────────────────────────────────────

struct MixerApp {
    audio_ep: u64,
    playing:  bool,
    waveform: Waveform,
    frequency: f32,
    volume:    f32,
    phase:     f32,
    buf:      Vec<i16>,
    preview:  Vec<f32>,
}

impl MixerApp {
    fn new() -> Self {
        // IMPORTANT: Vec::new() does NOT allocate — allocation happens lazily
        // in send_chunk() after bos_egui::run() has initialized the global allocator.
        MixerApp {
            audio_ep:  SVC_ERR_NOT_FOUND,
            playing:   false,
            waveform:  Waveform::Sine,
            frequency: 440.0,
            volume:    0.75,
            phase:     0.0,
            buf:       Vec::new(),
            preview:   Vec::new(),
        }
    }

    fn try_connect(&mut self) -> bool {
        if self.audio_ep != SVC_ERR_NOT_FOUND {
            return true;
        }
        let ep = ulib::sys_lookup_service(b"audio");
        if ep != SVC_ERR_NOT_FOUND {
            self.audio_ep = ep;
            self.sync_volume();
            return true;
        }
        false
    }

    fn sync_volume(&self) {
        // AC'97 attenuation: 0 = loudest, 100 = mute.
        let atten = ((1.0 - self.volume) * 100.0) as u8;
        ulib::audio::set_volume(self.audio_ep, atten);
    }

    fn send_chunk(&mut self) {
        if self.audio_ep == SVC_ERR_NOT_FOUND { return; }

        // Lazy allocation: the allocator is only valid after bos_egui::run() starts.
        let needed = CHUNK_FRAMES * CHANNELS as usize;
        if self.buf.len() != needed {
            self.buf.resize(needed, 0i16);
        }
        if self.preview.len() != PREVIEW_POINTS {
            self.preview.resize(PREVIEW_POINTS, 0.0f32);
        }

        let phase_inc = self.frequency / SAMPLE_RATE as f32;
        let amp = (self.volume * i16::MAX as f32) as i32;
        let preview_step = (CHUNK_FRAMES / PREVIEW_POINTS).max(1);

        for i in 0..CHUNK_FRAMES {
            let s = self.waveform.sample(self.phase);
            let s16 = ((s * amp as f32) as i32)
                .clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            self.buf[i * 2]     = s16;
            self.buf[i * 2 + 1] = s16;

            if i % preview_step == 0 {
                let idx = i / preview_step;
                if idx < PREVIEW_POINTS { self.preview[idx] = s; }
            }

            self.phase += phase_inc;
            if self.phase >= 1.0 { self.phase -= 1.0; }
        }

        ulib::audio::play_pcm(self.audio_ep, &self.buf, SAMPLE_RATE, CHANNELS);
    }
}

impl App for MixerApp {
    fn update(&mut self, ctx: &egui::Context) {
        // Audio tick — runs every frame while playing.
        if self.playing {
            self.send_chunk();
            bos_egui::request_timed_redraw(10);
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Mixer");
            ui.separator();

            // ── Connection status ─────────────────────────────────────────────
            let connected = self.try_connect();
            if connected {
                ui.colored_label(GREEN, "Sound server: connected");
            } else {
                ui.colored_label(RED, "Sound server: not found");
            }

            ui.separator();

            // ── Play / Stop ───────────────────────────────────────────────────
            ui.horizontal(|ui| {
                let btn_text = if self.playing { "Stop" } else { "Play" };
                if ui.button(btn_text).clicked() && connected {
                    self.playing = !self.playing;
                    if self.playing {
                        self.send_chunk();
                        self.send_chunk();
                    }
                }
                if self.playing {
                    ui.colored_label(GREEN, " Playing…");
                } else {
                    ui.colored_label(DIM, " Stopped");
                }
            });

            ui.separator();

            // ── Waveform selector ─────────────────────────────────────────────
            ui.label("Waveform:");
            ui.horizontal(|ui| {
                for &(wf, name) in Waveform::ALL {
                    if ui.selectable_label(self.waveform == wf, name).clicked() {
                        self.waveform = wf;
                        self.phase = 0.0;
                    }
                }
            });

            ui.separator();

            // ── Frequency slider ──────────────────────────────────────────────
            ui.label(format!("Frequency: {:.0} Hz", self.frequency));
            ui.add(egui::Slider::new(&mut self.frequency, 55.0..=4000.0).text("Hz"));

            // Common note shortcuts
            ui.horizontal(|ui| {
                for &(name, hz) in &[
                    ("A2", 110.0_f32), ("A3", 220.0), ("A4", 440.0), ("A5", 880.0),
                    ("C4", 261.6),     ("C5", 523.3),  ("C6", 1046.5),
                ] {
                    if ui.small_button(name).clicked() {
                        self.frequency = hz;
                    }
                }
            });

            ui.separator();

            // ── Volume slider ─────────────────────────────────────────────────
            ui.label(format!("Volume: {:.0}%", self.volume * 100.0));
            if ui.add(egui::Slider::new(&mut self.volume, 0.0..=1.0).text("Vol")).changed() && connected {
                self.sync_volume();
            }

            ui.separator();

            // ── Waveform preview canvas ───────────────────────────────────────
            ui.label("Preview:");
            let mut c = ui.fixed_canvas(64);
            let w = c.width;
            let h = c.height;
            let n = self.preview.len();

            // Background
            c.fill_rect(0, 0, w, h, DARK.into());

            // Grid: zero line and ±0.5 guides
            c.draw_hline(h / 2, GRID.into());
            c.draw_hline(h / 4, GRID.into());
            c.draw_hline(3 * h / 4, GRID.into());

            // Waveform: draw single-pixel columns
            if n > 1 && self.playing {
                for i in 0..n {
                    let x = (i as i32 * w) / n as i32;
                    let s = self.preview[i];                        // -1..1
                    let y = ((1.0 - s) * 0.5 * h as f32) as i32;  // map to canvas coords
                    let y = y.clamp(0, h - 1);
                    c.fill_rect(x, y, 1, 1, BLUE.into());
                    // draw a short vertical line so sparse samples are still visible
                    let mid = h / 2;
                    if y < mid {
                        c.fill_rect(x, y, 1, mid - y + 1, BLUE.into());
                    } else {
                        c.fill_rect(x, mid, 1, y - mid + 1, BLUE.into());
                    }
                }
            }
        });
    }
}

// ── Entry / panic ─────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point(_arg: u64) -> ! {
    bos_egui::run("Mixer", MixerApp::new())
}

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    ulib::default_panic(info)
}
