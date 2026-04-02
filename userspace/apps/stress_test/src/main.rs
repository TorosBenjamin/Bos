#![cfg_attr(not(target_os = "linux"), no_std)]
#![cfg_attr(not(target_os = "linux"), no_main)]

extern crate alloc;
use alloc::format;
use bos_egui::{App, egui};

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    ulib::default_panic(info)
}

struct PngRng {
    state: u64,
}

impl PngRng {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }
    fn next(&mut self) -> u32 {
        self.state = self.state.wrapping_mul(6364136223846793005).wrapping_add(1);
        (self.state >> 32) as u32
    }
    fn next_range(&mut self, min: u32, max: u32) -> u32 {
        if min >= max { return min; }
        min + (self.next() % (max - min))
    }
}

struct StressState {
    rng: PngRng,
    fs_ops: u32,
    ipc_msgs: u32,
    apps_spawned: u32,
    last_app: alloc::string::String,
    current_phase: &'static str,
    log: alloc::vec::Vec<alloc::string::String>,
    auto_mode: bool,
}

impl Default for StressState {
    fn default() -> Self {
        Self {
            rng: PngRng::new(ulib::sys_get_time_ns()),
            fs_ops: 0,
            ipc_msgs: 0,
            apps_spawned: 0,
            last_app: alloc::string::String::new(),
            current_phase: "Idle",
            log: alloc::vec::Vec::new(),
            auto_mode: true,
        }
    }
}

impl StressState {
    fn add_log(&mut self, msg: alloc::string::String) {
        ulib::log::write(ulib::log::LogLevel::Info, "stress", &msg);
        self.log.push(msg);
        if self.log.len() > 10 {
            self.log.remove(0);
        }
    }

    fn do_fs_op(&mut self) {
        let fs_fd = ulib::fs::fs_lookup();
        let op = self.rng.next_range(0, 5);
        let filename = format!("STRESS_{}.TXT", self.rng.next_range(0, 100));

        match op {
            0 => { // Create & Write
                let _ = ulib::fs::fs_create_file(fs_fd, &filename);
                let (buf_id, ptr) = ulib::sys_create_shared_buf(1024);
                if !ptr.is_null() {
                    let _ = ulib::fs::fs_write_file(fs_fd, &filename, buf_id, 1024);
                    ulib::sys_destroy_shared_buf(buf_id);
                }
            }
            1 => { // Read
                if let Some((buf_id, _size)) = ulib::fs::fs_map_file(fs_fd, &filename) {
                    ulib::sys_destroy_shared_buf(buf_id);
                }
            }
            2 => { // Append
                let (buf_id, ptr) = ulib::sys_create_shared_buf(512);
                if !ptr.is_null() {
                    let _ = ulib::fs::fs_append_file(fs_fd, &filename, buf_id, 512);
                    ulib::sys_destroy_shared_buf(buf_id);
                }
            }
            3 => { // Rename
                let new_name = format!("STRESS_R_{}.TXT", self.rng.next_range(0, 100));
                let _ = ulib::fs::fs_rename(fs_fd, &filename, &new_name);
            }
            4 => { // Remove
                let _ = ulib::fs::fs_rm(fs_fd, &filename);
            }
            _ => {}
        }
        ulib::handle::close(fs_fd);
        self.fs_ops += 1;
    }

    fn do_ipc_op(&mut self) {
        let (tx, rx) = match ulib::handle::channel(64) {
            Some(pair) => pair,
            None => return,
        };
        let buf = [0u8; 32];
        if ulib::handle::write(tx, &buf).is_some() {
            let mut recv_buf = [0u8; 32];
            let _ = ulib::handle::read(rx, &mut recv_buf);
        }
        ulib::handle::close(tx);
        ulib::handle::close(rx);
        self.ipc_msgs += 1;
    }

    fn do_spawn_op(&mut self) {
        let apps = ["SHELL.ELF", "DOOM.ELF", "BOSER.ELF", "FILES.ELF", "HELLO.ELF"];
        let app_name = apps[self.rng.next_range(0, apps.len() as u32) as usize];
        let fs_fd = ulib::fs::fs_lookup();

        if let Some((buf_id, size)) = ulib::fs::fs_map_file(fs_fd, app_name) {
            let ptr = ulib::sys_map_shared_buf(buf_id);
            if !ptr.is_null() {
                let elf_data = unsafe { core::slice::from_raw_parts(ptr, size as usize) };
                let _ = ulib::sys_spawn(elf_data, 0);
                ulib::sys_munmap(ptr, size);
                self.apps_spawned += 1;
                self.last_app = alloc::string::String::from(app_name);
                self.add_log(format!("Spawned {}", app_name));
            }
            ulib::sys_destroy_shared_buf(buf_id);
        }
        ulib::handle::close(fs_fd);
    }
}

impl App for StressState {
    fn update(&mut self, ctx: &egui::Context) {
        if self.auto_mode {
            self.current_phase = "Auto-Stress";
            // Do a few operations each frame
            for _ in 0..5 { self.do_fs_op(); }
            for _ in 0..10 { self.do_ipc_op(); }
            
            // Randomly spawn an app every ~100 frames
            if self.rng.next_range(0, 100) == 0 {
                self.do_spawn_op();
            }
            bos_egui::request_redraw(); // Keep it running
        } else {
            self.current_phase = "Manual/Paused";
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("BOS System Stress Test");
            ui.separator();

            ui.horizontal(|ui| {
                // Checkbox not available in stub_egui, using selectable_label or similar if available, 
                // but let's just use a button to toggle for now to be safe.
                if ui.button(if self.auto_mode { "Stop Auto-Pilot" } else { "Start Auto-Pilot" }).clicked() {
                    self.auto_mode = !self.auto_mode;
                }
                if ui.button("Step FS").clicked() { self.do_fs_op(); }
                if ui.button("Step IPC").clicked() { self.do_ipc_op(); }
                if ui.button("Spawn Random App").clicked() { self.do_spawn_op(); }
            });

            ui.separator();
            ui.label(format!("FS Operations: {}", self.fs_ops));
            ui.label(format!("IPC Messages: {}", self.ipc_msgs));
            ui.label(format!("Apps Spawned: {}", self.apps_spawned));
            ui.label(format!("Last App: {}", self.last_app));

            ui.separator();
            ui.label(format!("Phase: {}", self.current_phase));
            
            ui.label("Log:");
            for line in &self.log {
                ui.label(line);
            }

            if ui.button("Shutdown System").clicked() {
                ulib::sys_shutdown(0);
            }
        });
    }
}

#[unsafe(no_mangle)]
pub extern "sysv64" fn entry_point(_arg: u64) -> ! {
    bos_egui::run("Stress Test Dashboard", StressState::default())
}
