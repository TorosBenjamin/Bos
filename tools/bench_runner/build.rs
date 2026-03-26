use std::fs::create_dir_all;
use std::io::{Cursor, ErrorKind, Seek, SeekFrom};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::{env, io};

/// Binaries in the ISO root.
/// bench_init replaces init_task (same path: /init_task so the kernel loads it).
/// All benchmark binaries are loaded by bench_init via sys_get_module().
const ISO_BINARIES: &[(&str, &str, &str)] = &[
    ("bench_init",      "bench_init",      "init_task"),   // replaces real init_task
    ("ipc_bench",       "ipc_bench",       "ipc_bench"),
    ("syscall_bench",   "syscall_bench",   "syscall_bench"),
    ("ctx_switch_bench","ctx_switch_bench", "ctx_switch_bench"),
    ("mem_bench",       "mem_bench",       "mem_bench"),
];

fn main() {
    check_command_exists("xorriso");
    check_command_exists("limine");

    let out_dir    = PathBuf::from(env::var("OUT_DIR").unwrap());
    let runner_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let limine_dir = env::var("LIMINE_PATH").map(PathBuf::from).unwrap_or_else(|_| {
        let workspace_root = runner_dir.ancestors().nth(2)
            .expect("could not determine workspace root")
            .to_path_buf();
        let default = workspace_root.join("limine");
        assert!(
            default.join("BOOTX64.EFI").exists(),
            "LIMINE_PATH not set and limine submodule not initialized."
        );
        default
    });

    // ── ISO root ──────────────────────────────────────────────────────────────
    let iso_dir = out_dir.join("iso_root");
    create_dir_all(&iso_dir).unwrap();

    // limine.conf — no test suite, no cmdline
    std::fs::write(
        iso_dir.join("limine.conf"),
        "TIMEOUT 0\nDEFAULT_ENTRY 0\n\n/Bos\n    protocol: limine\n    kernel_path: boot():/kernel\n",
    ).unwrap();

    // Kernel binary
    let kernel_bin = artifact_bin("kernel", "kernel");
    ensure_symlink(&kernel_bin, iso_dir.join("kernel")).unwrap();

    // Benchmark binaries
    for &(dep, bin, iso_name) in ISO_BINARIES {
        ensure_symlink(artifact_bin(dep, bin), iso_dir.join(iso_name)).unwrap();
    }

    // Limine boot files
    let limine_out = iso_dir.join("boot/limine");
    create_dir_all(&limine_out).unwrap();
    for file in ["limine-bios.sys", "limine-bios-cd.bin", "limine-uefi-cd.bin"] {
        ensure_symlink(limine_dir.join(file), limine_out.join(file)).unwrap();
    }
    let efi_dir = iso_dir.join("EFI/BOOT");
    create_dir_all(&efi_dir).unwrap();
    for file in ["BOOTX64.EFI", "BOOTIA32.EFI"] {
        ensure_symlink(limine_dir.join(file), efi_dir.join(file)).unwrap();
    }

    // Empty FAT32 disk image (kernel expects one, even if unused)
    let disk_img = out_dir.join("bench_disk.img");
    create_empty_fat32(&disk_img);
    println!("cargo:rustc-env=DISK_IMG={}", disk_img.display());

    // Build the ISO
    let iso_path = out_dir.join("bench.iso");
    xorriso_mkisofs(&iso_dir, &limine_out, &iso_path);
    limine_bios_install(&iso_path);
    println!("cargo:rustc-env=ISO={}", iso_path.display());
}

fn artifact_bin(dep: &str, bin: &str) -> String {
    let dep_upper = dep.to_ascii_uppercase().replace('-', "_");
    let key = format!("CARGO_BIN_FILE_{dep_upper}_{bin}");
    env::var(&key).unwrap_or_else(|_| {
        panic!("Artifact binary not found: {key}")
    })
}

fn xorriso_mkisofs(iso_dir: &Path, limine_out: &Path, output: &Path) {
    let status = std::process::Command::new("xorriso")
        .args(["-as", "mkisofs", "--follow-links"])
        .arg("-b").arg(limine_out.join("limine-bios-cd.bin").strip_prefix(iso_dir).unwrap())
        .args(["-no-emul-boot", "-boot-load-size", "4", "-boot-info-table"])
        .arg("--efi-boot").arg(limine_out.join("limine-uefi-cd.bin").strip_prefix(iso_dir).unwrap())
        .args(["-efi-boot-part", "--efi-boot-image", "--protective-msdos-label"])
        .arg(iso_dir)
        .arg("-o").arg(output)
        .stderr(Stdio::inherit()).stdout(Stdio::inherit())
        .status().unwrap();
    assert!(status.success(), "xorriso failed");
}

fn limine_bios_install(iso: &Path) {
    let status = std::process::Command::new("limine")
        .args(["bios-install"]).arg(iso)
        .stderr(Stdio::inherit()).stdout(Stdio::inherit())
        .status().unwrap();
    assert!(status.success(), "limine bios-install failed");
}

fn ensure_symlink<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> io::Result<()> {
    match std::fs::remove_file(&link) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }?;
    symlink(original, link)
}

fn check_command_exists(cmd: &str) {
    if std::process::Command::new(cmd).arg("--version").output().is_err() {
        panic!("Command '{cmd}' not found. Please install it.");
    }
}

fn create_empty_fat32(path: &Path) {
    const DISK_SIZE: u64 = 64 * 1024 * 1024;
    let mut disk: Cursor<Vec<u8>> = Cursor::new(vec![0u8; DISK_SIZE as usize]);
    fatfs::format_volume(
        &mut disk,
        fatfs::FormatVolumeOptions::new()
            .volume_label(*b"BENCH      ")
            .fat_type(fatfs::FatType::Fat32),
    ).expect("fatfs: format_volume failed");
    disk.seek(SeekFrom::Start(0)).unwrap();
    std::fs::write(path, disk.into_inner()).expect("failed to write bench disk image");
}
