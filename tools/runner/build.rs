use std::fs::{create_dir_all, remove_file};
use std::io::{ErrorKind, Cursor, Seek, SeekFrom, Write};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::{env, io};

// ── Tables — edit these to add / remove binaries ──────────────────────────────

/// Every userspace binary that lands in the ISO root.
/// Format: (cargo dep name, binary name, filename in ISO root)
/// Cargo sets CARGO_BIN_FILE_{DEP_UPPER}_{bin} for each artifact dependency.
const ISO_BINARIES: &[(&str, &str, &str)] = &[
    ("init_task",      "init_task",      "init_task"),
    ("display_server", "display_server", "display_server"),
    ("fs_server",      "fs_server",      "fs_server"),
    ("e1000",          "e1000",          "e1000"),
    ("net_server",     "net_server",     "net_server"),
];

/// Binaries written into the FAT32 disk image for fs_server to load at runtime.
/// Format: (cargo dep name, binary name, 8.3 FAT filename)
const FAT32_BINARIES: &[(&str, &str, &str)] = &[
    ("user_land",  "bouncing_cube_1", "CUBE1.ELF"),
    ("user_land",  "bouncing_cube_2", "CUBE2.ELF"),
    ("hello_egui", "hello_egui",      "HELLO.ELF"),
    ("files",      "files",           "FILES.ELF"),
    ("launcher",   "launcher",        "LAUNCH.ELF"),
];

/// Kernel test feature flags → test suite name passed on the kernel cmdline.
const TEST_FEATURES: &[(&str, &str)] = &[
    ("CARGO_FEATURE_TEST_MEM",         "mem"),
    ("CARGO_FEATURE_TEST_TIME",        "time"),
    ("CARGO_FEATURE_TEST_INTERRUPTS",  "interrupts"),
    ("CARGO_FEATURE_TEST_GRAPHICS",    "graphics"),
    ("CARGO_FEATURE_TEST_USERMODE",    "usermode"),
    ("CARGO_FEATURE_TEST_KEYBOARD",    "keyboard"),
    ("CARGO_FEATURE_TEST_IPC",         "ipc"),
    ("CARGO_FEATURE_TEST_DISPLAY",     "display"),
    ("CARGO_FEATURE_TEST_SCHEDULER",   "scheduler"),
    ("CARGO_FEATURE_TEST_ELF",         "elf"),
    ("CARGO_FEATURE_TEST_SCHED",       "sched"),
    ("CARGO_FEATURE_TEST_SCHED_NOELF", "sched-noelf"),
];

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() {
    check_command_exists("xorriso");
    check_command_exists("limine");

    let out_dir    = PathBuf::from(env::var("OUT_DIR").unwrap());
    let runner_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let limine_dir = env::var("LIMINE_PATH").map(PathBuf::from)
        .expect("LIMINE_PATH not set — point it at the directory containing BOOTX64.EFI etc.");

    // ── ISO root ──────────────────────────────────────────────────────────────
    let iso_dir = out_dir.join("iso_root");
    create_dir_all(&iso_dir).unwrap();

    // limine.conf (optionally with test_suite= cmdline)
    let test_suite = TEST_FEATURES.iter()
        .find_map(|&(feat, name)| env::var(feat).ok().map(|_| name));
    let cmdline = test_suite
        .map(|s| format!("    cmdline: test_suite={s}\n"))
        .unwrap_or_default();
    std::fs::write(
        iso_dir.join("limine.conf"),
        format!("TIMEOUT 0\nDEFAULT_ENTRY 0\n\n/Bos\n    protocol: limine\n    kernel_path: boot():/kernel\n{cmdline}"),
    ).unwrap();

    // Kernel (or test kernel)
    let kernel_bin = if env::var("CARGO_FEATURE_KERNEL_TEST").is_ok() {
        artifact_bin("tests", "tests")
    } else {
        artifact_bin("kernel", "kernel")
    };
    ensure_symlink(&kernel_bin, iso_dir.join("kernel")).unwrap();

    // Userspace binaries
    for &(dep, bin, iso_name) in ISO_BINARIES {
        ensure_symlink(artifact_bin(dep, bin), iso_dir.join(iso_name)).unwrap();
    }

    // Optional integration-test binary
    if env::var("CARGO_FEATURE_USERSPACE_TEST").is_ok() {
        ensure_symlink(artifact_bin("utest", "utest"), iso_dir.join("utest")).unwrap();
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

    // FAT32 disk image
    let disk_img = out_dir.join("disk.img");
    create_fat32_disk_image(&disk_img, &runner_dir);
    println!("cargo:rustc-env=DISK_IMG={}", disk_img.display());

    // Re-run if configs change
    println!("cargo:rerun-if-changed=bos_ds.conf");
    println!("cargo:rerun-if-changed=launcher.conf");

    // Stable symlink to out_dir for convenient inspection
    ensure_symlink(&out_dir, runner_dir.join("out_dir")).unwrap();

    // Build the ISO
    let iso_path = out_dir.join("os.iso");
    xorriso_mkisofs(&iso_dir, &limine_out, &iso_path);
    limine_bios_install(&iso_path);
    println!("cargo:rustc-env=ISO={}", iso_path.display());
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Resolve a Cargo artifact binary path from the environment.
///
/// Cargo sets `CARGO_BIN_FILE_{DEP_UPPER}_{bin}` for every artifact dependency,
/// where DEP_UPPER is the dependency name uppercased with hyphens→underscores.
fn artifact_bin(dep: &str, bin: &str) -> String {
    let dep_upper = dep.to_ascii_uppercase().replace('-', "_");
    let key = format!("CARGO_BIN_FILE_{dep_upper}_{bin}");
    env::var(&key).unwrap_or_else(|_| {
        panic!("Artifact binary not found: {key}\nIs '{dep}' declared as an artifact build-dependency in runner/Cargo.toml?")
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

pub fn ensure_symlink<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> io::Result<()> {
    match remove_file(&link) {
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

// ── FAT32 disk image ──────────────────────────────────────────────────────────

fn create_fat32_disk_image(path: &Path, runner_dir: &Path) {
    const DISK_SIZE: u64 = 64 * 1024 * 1024; // 64 MB

    let mut disk: Cursor<Vec<u8>> = Cursor::new(vec![0u8; DISK_SIZE as usize]);
    fatfs::format_volume(
        &mut disk,
        fatfs::FormatVolumeOptions::new()
            .volume_label(*b"BOS_APPS   ")
            .fat_type(fatfs::FatType::Fat32),
    ).expect("fatfs: format_volume failed");

    disk.seek(SeekFrom::Start(0)).unwrap();
    {
        let fs = fatfs::FileSystem::new(&mut disk, fatfs::FsOptions::new())
            .expect("fatfs: FileSystem::new failed");
        let root = fs.root_dir();

        for &(dep, bin, fat_name) in FAT32_BINARIES {
            let bin_path = artifact_bin(dep, bin);
            match std::fs::read(&bin_path) {
                Ok(data) => {
                    let mut f = root.create_file(fat_name).expect("fatfs: create_file failed");
                    f.truncate().unwrap();
                    f.write_all(&data).unwrap();
                }
                Err(e) => eprintln!("build.rs: skipping {fat_name}: {e}"),
            }
        }

        let conf_src = runner_dir.join("bos_ds.conf");
        let config = std::fs::read(&conf_src)
            .unwrap_or_else(|e| panic!("build.rs: cannot read {}: {e}", conf_src.display()));
        root.create_file("bos_ds.conf").expect("fatfs: create bos_ds.conf")
            .write_all(&config).unwrap();

        // Launcher config
        let launcher_conf = std::fs::read(runner_dir.join("launcher.conf")).unwrap_or_default();
        root.create_file("LAUNCH.CFG").expect("fatfs: create LAUNCH.CFG")
            .write_all(&launcher_conf).unwrap();
    }

    std::fs::write(path, disk.into_inner()).expect("build.rs: failed to write disk.img");
    println!("build.rs: wrote {} MB disk image to {}", DISK_SIZE / 1_048_576, path.display());
}
