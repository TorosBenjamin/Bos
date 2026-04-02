use alloc::{format, string::String, vec::Vec};

use crate::{ShellApp, Line, PROMPT_COLOR, DIR_COLOR, DIM_COLOR};

impl ShellApp {
    // ── Command dispatch ──────────────────────────────────────────────────────

    pub(crate) fn execute(&mut self, cmd_line: &str) {
        self.push_line(Line::colored(format!("{}{}", self.prompt_str(), cmd_line), PROMPT_COLOR));

        if cmd_line.contains('|') {
            self.execute_pipeline(cmd_line);
            return;
        }

        let parts: Vec<&str> = cmd_line.split_whitespace().collect();
        if parts.is_empty() { return; }

        match parts[0] {
            "help"   => self.cmd_help(),
            "echo"   => self.cmd_echo(&parts[1..]),
            "clear"  => self.cmd_clear(),
            "time"   => self.cmd_time(),
            "ls"     => self.cmd_ls(parts.get(1).copied()),
            "cat"    => self.cmd_cat(parts.get(1).copied()),
            "stat"   => self.cmd_stat(parts.get(1).copied()),
            "run"    => self.cmd_run(parts.get(1).copied()),
            "cd"     => self.cmd_cd(parts.get(1).copied()),
            "mkdir"  => self.cmd_mkdir(parts.get(1).copied()),
            "touch"  => self.cmd_touch(parts.get(1).copied()),
            "rm"     => self.cmd_rm(parts.get(1).copied()),
            "mv"     => self.cmd_mv(parts.get(1).copied(), parts.get(2).copied()),
            "edit"   => self.cmd_edit(parts.get(1).copied()),
            "append" => self.cmd_append(parts.get(1).copied(), &parts[2.min(parts.len())..]),
            "syslog" => self.cmd_syslog(parts.get(1).copied()),
            other    => self.push_err(format!("unknown command: {}", other)),
        }
    }

    // ── Commands ──────────────────────────────────────────────────────────────

    fn cmd_help(&mut self) {
        self.push_normal(String::from("Available commands:"));
        self.push_normal(String::from("  help                   Show this help"));
        self.push_normal(String::from("  echo <text>            Print text"));
        self.push_normal(String::from("  clear                  Clear screen"));
        self.push_normal(String::from("  time                   Show time (seconds since epoch)"));
        self.push_normal(String::from("  ls [path]              List directory contents"));
        self.push_normal(String::from("  cat <path>             Print file contents"));
        self.push_normal(String::from("  stat <path>            Show file metadata"));
        self.push_normal(String::from("  run <path>             Launch an ELF from the filesystem"));
        self.push_normal(String::from("  cd <path>              Change working directory"));
        self.push_normal(String::from("  mkdir <path>           Create a directory"));
        self.push_normal(String::from("  touch <path>           Create an empty file"));
        self.push_normal(String::from("  rm <path>              Delete a file"));
        self.push_normal(String::from("  mv <old> <new>         Rename a file or directory"));
        self.push_normal(String::from("  edit <path>            Open text editor"));
        self.push_normal(String::from("  append <path> <text>   Append text (+ newline) to a file"));
        self.push_normal(String::from("  syslog [N]             Show last N log entries (default 50)"));
    }

    fn cmd_echo(&mut self, args: &[&str]) {
        let mut out = String::new();
        for (i, a) in args.iter().enumerate() {
            if i > 0 { out.push(' '); }
            out.push_str(a);
        }
        self.push_normal(out);
    }

    pub(crate) fn cmd_clear(&mut self) {
        self.lines.clear();
        self.scroll_offset = 0;
        self.auto_scroll = true;
    }

    fn cmd_time(&mut self) {
        let ns = ulib::sys_get_time_ns();
        let secs = ns / 1_000_000_000;
        let h = (secs / 3600) % 24;
        let m = (secs % 3600) / 60;
        let s = secs % 60;
        self.push_normal(format!("{:02}:{:02}:{:02} UTC ({}s since epoch)", h, m, s, secs));
    }

    fn cmd_ls(&mut self, path: Option<&str>) {
        let path = match path {
            Some(p) => self.resolve_path(p),
            None    => self.cwd.clone(),
        };
        let fs_fd = self.ensure_fs();
        match ulib::fs::fs_readdir(fs_fd, &path) {
            Some(resp) => {
                for i in 0..(resp.count as usize) {
                    let entry = &resp.entries[i];
                    let name_len = (entry.name_len as usize).min(entry.name.len());
                    let name = core::str::from_utf8(&entry.name[..name_len]).unwrap_or("???");
                    if entry.is_dir != 0 {
                        self.push_line(Line::colored(format!("  <DIR>     {}", name), DIR_COLOR));
                    } else {
                        self.push_line(Line::colored(format!("  {:>7}   {}", entry.size, name), DIM_COLOR));
                    }
                }
                if resp.count == 0 {
                    self.push_normal(String::from("  (empty)"));
                }
            }
            None => self.push_err(format!("ls: cannot read '{}'", path)),
        }
    }

    fn cmd_cat(&mut self, path: Option<&str>) {
        let path = match path {
            Some(p) => p,
            None => { self.push_err(String::from("cat: missing path")); return; }
        };
        let resolved = self.resolve_path(path);
        let fs_fd = self.ensure_fs();
        match ulib::fs::fs_map_file(fs_fd, &resolved) {
            Some((buf_id, file_size)) => {
                let ptr = ulib::sys_map_shared_buf(buf_id);
                if ptr.is_null() {
                    self.push_err(String::from("cat: failed to map file"));
                    ulib::sys_destroy_shared_buf(buf_id);
                    return;
                }
                let cap = (file_size as usize).min(64 * 1024);
                let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, cap) };
                for line in bytes.split(|&b| b == b'\n') {
                    let s = core::str::from_utf8(line).unwrap_or("(binary data)");
                    self.push_normal(String::from(s.trim_end_matches('\r')));
                }
                if file_size > 64 * 1024 {
                    self.push_line(Line::colored(
                        format!("... truncated ({} bytes total)", file_size), DIM_COLOR,
                    ));
                }
                ulib::sys_munmap(ptr, file_size);
                ulib::sys_destroy_shared_buf(buf_id);
            }
            None => self.push_err(format!("cat: file not found: '{}'", resolved)),
        }
    }

    fn cmd_stat(&mut self, path: Option<&str>) {
        let path = match path {
            Some(p) => p,
            None => { self.push_err(String::from("stat: missing path")); return; }
        };
        let resolved = self.resolve_path(path);
        let fs_fd = self.ensure_fs();
        match ulib::fs::fs_stat(fs_fd, &resolved) {
            Some(resp) => {
                let kind = if resp.is_dir != 0 { "directory" } else { "file" };
                self.push_normal(format!("  type: {}", kind));
                self.push_normal(format!("  size: {} bytes", resp.size));
            }
            None => self.push_err(format!("stat: not found: '{}'", resolved)),
        }
    }

    fn cmd_run(&mut self, path: Option<&str>) {
        if self.active_child.is_some() {
            self.push_err(String::from("run: a child is already running (Ctrl+C to detach)"));
            return;
        }
        let path = match path {
            Some(p) => p,
            None => { self.push_err(String::from("run: missing ELF path")); return; }
        };
        let resolved = self.resolve_path(path);
        let fs_fd = self.ensure_fs();

        let Some((buf_id, elf_size)) = ulib::fs::fs_map_file(fs_fd, &resolved) else {
            self.push_err(format!("run: file not found: '{}'", resolved));
            return;
        };
        let elf_ptr = ulib::sys_map_shared_buf(buf_id);
        if elf_ptr.is_null() {
            ulib::sys_destroy_shared_buf(buf_id);
            self.push_err(String::from("run: failed to map ELF"));
            return;
        }

        let (stdin_r, stdin_w) = match ulib::handle::pipe() {
            Some(p) => p,
            None => {
                ulib::sys_munmap(elf_ptr, elf_size);
                ulib::sys_destroy_shared_buf(buf_id);
                self.push_err(String::from("run: pipe() failed"));
                return;
            }
        };
        let (stdout_r, stdout_w) = match ulib::handle::pipe() {
            Some(p) => p,
            None => {
                ulib::handle::close(stdin_r);
                ulib::handle::close(stdin_w);
                ulib::sys_munmap(elf_ptr, elf_size);
                ulib::sys_destroy_shared_buf(buf_id);
                self.push_err(String::from("run: pipe() failed"));
                return;
            }
        };

        let elf_bytes = unsafe {
            core::slice::from_raw_parts(elf_ptr as *const u8, elf_size as usize)
        };
        let task_id = ulib::handle::spawn_with_handles(
            elf_bytes,
            0,
            path.as_bytes(),
            kernel_api_types::Priority::Normal as u8,
            &[(stdin_r, 0), (stdout_w, 1), (stdout_w, 2)],
        );
        ulib::sys_munmap(elf_ptr, elf_size);
        ulib::sys_destroy_shared_buf(buf_id);
        ulib::handle::close(stdin_r);
        ulib::handle::close(stdout_w);

        if task_id == 0 {
            ulib::handle::close(stdin_w);
            ulib::handle::close(stdout_r);
            self.push_err(format!("run: failed to spawn '{}'", path));
        } else {
            ulib::sys_wait_task_ready(task_id);
            self.push_normal(format!("spawned '{}' (task {})", path, task_id));
            self.active_child = Some(crate::ChildState {
                stdout_read_fd: stdout_r,
                stdin_write_fd: stdin_w,
                input_echo:     String::new(),
            });
        }
    }

    fn cmd_cd(&mut self, path: Option<&str>) {
        let path = match path {
            Some(p) => p,
            None => { self.cwd = String::from("/"); return; }
        };
        let resolved = self.resolve_path(path);
        let fs_fd = self.ensure_fs();
        match ulib::fs::fs_stat(fs_fd, &resolved) {
            Some(resp) if resp.is_dir != 0 => { self.cwd = resolved; }
            Some(_) => self.push_err(format!("cd: not a directory: '{}'", path)),
            None    => self.push_err(format!("cd: no such directory: '{}'", path)),
        }
    }

    fn cmd_mkdir(&mut self, path: Option<&str>) {
        let path = match path {
            Some(p) => p,
            None => { self.push_err(String::from("mkdir: missing path")); return; }
        };
        let resolved = self.resolve_path(path);
        let fs_fd = self.ensure_fs();
        match ulib::fs::fs_mkdir(fs_fd, &resolved) {
            ulib::fs::FsResult::Ok => {}
            _ => self.push_err(format!("mkdir: failed to create '{}'", path)),
        }
    }

    fn cmd_touch(&mut self, path: Option<&str>) {
        let path = match path {
            Some(p) => p,
            None => { self.push_err(String::from("touch: missing path")); return; }
        };
        let resolved = self.resolve_path(path);
        let fs_fd = self.ensure_fs();
        match ulib::fs::fs_create_file(fs_fd, &resolved) {
            ulib::fs::FsResult::Ok => {}
            _ => self.push_err(format!("touch: failed to create '{}'", path)),
        }
    }

    fn cmd_rm(&mut self, path: Option<&str>) {
        let path = match path {
            Some(p) => p,
            None => { self.push_err(String::from("rm: missing path")); return; }
        };
        let resolved = self.resolve_path(path);
        let fs_fd = self.ensure_fs();
        match ulib::fs::fs_rm(fs_fd, &resolved) {
            ulib::fs::FsResult::Ok => {}
            ulib::fs::FsResult::NotFound => self.push_err(format!("rm: no such file: '{}'", path)),
            ulib::fs::FsResult::IsDir    => self.push_err(format!("rm: is a directory: '{}'", path)),
            _ => self.push_err(format!("rm: failed to remove '{}'", path)),
        }
    }

    fn cmd_mv(&mut self, old: Option<&str>, new: Option<&str>) {
        let (old, new) = match (old, new) {
            (Some(o), Some(n)) => (o, n),
            _ => { self.push_err(String::from("mv: usage: mv <old> <new>")); return; }
        };
        let resolved_old = self.resolve_path(old);
        let fs_fd = self.ensure_fs();
        match ulib::fs::fs_rename(fs_fd, &resolved_old, new) {
            ulib::fs::FsResult::Ok => {}
            ulib::fs::FsResult::NotFound => self.push_err(format!("mv: no such file: '{}'", old)),
            _ => self.push_err(format!("mv: failed to rename '{}'", old)),
        }
    }

    fn cmd_append(&mut self, path: Option<&str>, text_parts: &[&str]) {
        let path = match path {
            Some(p) => p,
            None => { self.push_err(String::from("append: usage: append <path> <text>")); return; }
        };
        let resolved = self.resolve_path(path);
        let fs_fd = self.ensure_fs();

        let mut line = String::new();
        for (i, part) in text_parts.iter().enumerate() {
            if i > 0 { line.push(' '); }
            line.push_str(part);
        }
        line.push('\n');

        let data = line.as_bytes();
        let (buf_id, ptr) = ulib::sys_create_shared_buf(data.len() as u64);
        if ptr.is_null() || buf_id == u64::MAX {
            self.push_err(String::from("append: out of memory"));
            return;
        }
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len()); }
        let result = ulib::fs::fs_append_file(fs_fd, &resolved, buf_id, data.len() as u64);
        ulib::sys_destroy_shared_buf(buf_id);

        match result {
            ulib::fs::FsResult::Ok => {}
            _ => self.push_err(format!("append: failed to write '{}'", path)),
        }
    }

    fn cmd_syslog(&mut self, count_arg: Option<&str>) {
        let max: u32 = count_arg
            .and_then(|s| {
                let mut n: u32 = 0;
                for b in s.bytes() {
                    if !b.is_ascii_digit() { return None; }
                    n = n.saturating_mul(10).saturating_add((b - b'0') as u32);
                }
                Some(n)
            })
            .unwrap_or(50);

        match ulib::log::read(max) {
            None => {
                self.push_err(String::from("syslog: logd not available"));
            }
            Some((_, 0)) => {
                self.push_normal(String::from("(no log entries)"));
            }
            Some((buf_id, count)) => {
                let ptr = ulib::sys_map_shared_buf(buf_id);
                if ptr.is_null() {
                    ulib::sys_destroy_shared_buf(buf_id);
                    self.push_err(String::from("syslog: failed to map buffer"));
                    return;
                }
                if count == 0 {
                    self.push_normal(String::from("(no log entries)"));
                }
                let entry_size = core::mem::size_of::<ulib::log::LogEntry>();
                for i in 0..count as usize {
                    let entry = unsafe {
                        &*(ptr.add(i * entry_size) as *const ulib::log::LogEntry)
                    };
                    let ns   = entry.timestamp;
                    let secs = ns / 1_000_000_000;
                    let h    = (secs / 3600) % 24;
                    let m    = (secs % 3600) / 60;
                    let s    = secs % 60;
                    let level = ulib::log::LogLevel::from_u8(entry.level).as_str();
                    let src = core::str::from_utf8(
                        &entry.source[..entry.source_len as usize]
                    ).unwrap_or("?");
                    let msg = core::str::from_utf8(
                        &entry.message[..entry.msg_len as usize]
                    ).unwrap_or("?");
                    self.push_normal(format!("[{:02}:{:02}:{:02}] [{}] [{}] {}", h, m, s, level, src, msg));
                }
                ulib::sys_munmap(ptr, (count as u64) * entry_size as u64);
                ulib::sys_destroy_shared_buf(buf_id);
            }
        }
    }

    // ── Pipeline ──────────────────────────────────────────────────────────────

    fn execute_pipeline(&mut self, cmd_line: &str) {
        if self.active_child.is_some() {
            self.push_err(String::from("pipe: a child is already running (Ctrl+C to detach)"));
            return;
        }

        let segments: Vec<&str> = cmd_line.split('|').collect();
        let mut paths: Vec<&str> = Vec::new();
        for seg in &segments {
            let parts: Vec<&str> = seg.split_whitespace().collect();
            if parts.len() < 2 || parts[0] != "run" {
                self.push_err(String::from("pipe: only `run <elf>` segments are supported"));
                return;
            }
            paths.push(parts[1]);
        }

        let n = paths.len();

        // Create n-1 inter-process connecting pipes
        let mut connecting: Vec<(u32, u32)> = Vec::new();
        for _ in 0..n - 1 {
            match ulib::handle::pipe() {
                Some(p) => connecting.push(p),
                None => {
                    for (r, w) in &connecting { ulib::handle::close(*r); ulib::handle::close(*w); }
                    self.push_err(String::from("pipe: failed to create pipe"));
                    return;
                }
            }
        }

        // Shell ↔ first program stdin
        let (first_stdin_r, first_stdin_w) = match ulib::handle::pipe() {
            Some(p) => p,
            None => {
                for (r, w) in &connecting { ulib::handle::close(*r); ulib::handle::close(*w); }
                self.push_err(String::from("pipe: failed to create pipe"));
                return;
            }
        };

        // Last program stdout ↔ shell
        let (last_stdout_r, last_stdout_w) = match ulib::handle::pipe() {
            Some(p) => p,
            None => {
                for (r, w) in &connecting { ulib::handle::close(*r); ulib::handle::close(*w); }
                ulib::handle::close(first_stdin_r);
                ulib::handle::close(first_stdin_w);
                self.push_err(String::from("pipe: failed to create pipe"));
                return;
            }
        };

        let fs_fd = self.ensure_fs();
        for (i, path) in paths.iter().enumerate() {
            let resolved = self.resolve_path(path);
            let Some((buf_id, elf_size)) = ulib::fs::fs_map_file(fs_fd, &resolved) else {
                self.push_err(format!("pipe: not found: '{}'", path));
                return;
            };
            let elf_ptr = ulib::sys_map_shared_buf(buf_id);
            if elf_ptr.is_null() {
                ulib::sys_destroy_shared_buf(buf_id);
                self.push_err(String::from("pipe: failed to map ELF"));
                return;
            }
            let elf_bytes = unsafe {
                core::slice::from_raw_parts(elf_ptr as *const u8, elf_size as usize)
            };

            let stdin_r  = if i == 0     { first_stdin_r       } else { connecting[i - 1].0 };
            let stdout_w = if i == n - 1 { last_stdout_w        } else { connecting[i].1     };

            let task_id = ulib::handle::spawn_with_handles(
                elf_bytes,
                0,
                path.as_bytes(),
                kernel_api_types::Priority::Normal as u8,
                &[(stdin_r, 0), (stdout_w, 1), (stdout_w, 2)],
            );
            ulib::sys_munmap(elf_ptr, elf_size);
            ulib::sys_destroy_shared_buf(buf_id);
            ulib::handle::close(stdin_r);
            ulib::handle::close(stdout_w);

            if task_id == 0 {
                self.push_err(format!("pipe: spawn failed for '{}'", path));
                return;
            }
        }

        self.push_normal(format!("pipeline spawned ({} tasks)", n));
        self.active_child = Some(crate::ChildState {
            stdout_read_fd: last_stdout_r,
            stdin_write_fd: first_stdin_w,
            input_echo:     String::new(),
        });
    }
}
