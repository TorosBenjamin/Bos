//! Read log files from the Bos FAT32 disk image.
//!
//! Usage:
//!   cargo logs              — print all source logs
//!   cargo logs logd         — print only logs/logd.log
//!   cargo logs logd init    — print logs/logd.log and logs/init.log

use std::io::Read;
use std::path::PathBuf;

fn main() {
    let filters: Vec<String> = std::env::args().skip(1).map(|s| s.to_ascii_lowercase()).collect();

    let disk_path = find_disk_img();
    let data = std::fs::read(&disk_path)
        .unwrap_or_else(|e| { eprintln!("error: cannot read {}: {e}", disk_path.display()); std::process::exit(1); });

    let cursor = std::io::Cursor::new(data);
    let fs = fatfs::FileSystem::new(cursor, fatfs::FsOptions::new())
        .unwrap_or_else(|e| { eprintln!("error: cannot open FAT32 image: {e}"); std::process::exit(1); });

    let logs_dir = match fs.root_dir().open_dir("logs") {
        Ok(d) => d,
        Err(_) => {
            eprintln!("No logs/ directory found. Run the OS first.");
            std::process::exit(1);
        }
    };

    let mut printed = 0usize;

    for entry in logs_dir.iter().flatten() {
        let name = entry.file_name();
        if name.starts_with('.') || !entry.is_file() { continue; }

        // Strip extension for filter comparison: "LOGD.LOG" → "logd"
        let stem = name
            .rsplit_once('.')
            .map(|(s, _)| s)
            .unwrap_or(&name)
            .to_ascii_lowercase();

        if !filters.is_empty() && !filters.contains(&stem) {
            continue;
        }

        let mut file = entry.to_file();
        let mut content = String::new();
        if file.read_to_string(&mut content).is_err() { continue; }
        if content.is_empty() { continue; }

        println!("=== {} ===", name.to_ascii_lowercase().replace(".log", ""));
        print!("{content}");
        if !content.ends_with('\n') { println!(); }
        printed += 1;
    }

    if printed == 0 {
        if filters.is_empty() {
            eprintln!("No log files found in logs/.");
        } else {
            eprintln!("No log files matched: {}", filters.join(", "));
        }
    }
}

fn find_disk_img() -> PathBuf {
    // runner/build.rs creates a stable symlink: tools/runner/out_dir -> OUT_DIR
    // disk.img lives at OUT_DIR/disk.img, so tools/runner/out_dir/disk.img always works.
    let candidates = [
        "tools/runner/out_dir/disk.img",
        "os/tools/runner/out_dir/disk.img",
    ];
    for path in &candidates {
        let p = PathBuf::from(path);
        if p.exists() { return p; }
    }
    eprintln!(
        "disk.img not found (tried: {}).\nRun `cargo run -p runner` first to create it.",
        candidates.join(", ")
    );
    std::process::exit(1);
}
