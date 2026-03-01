use std::fs::{create_dir_all, remove_file};
use std::io::{ErrorKind, Cursor, Seek, SeekFrom, Write};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::{env, io};

fn main() {
    check_command_exists("xorriso");
    check_command_exists("limine");
    // This is the folder where a build script (this file) should place its output
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    // This is the `runner` folder
    let runner_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    // This folder contains Limine files such as `BOOTX64.EFI`
    let limine_dir = match env::var("LIMINE_PATH") {
        Ok(path) => PathBuf::from(path),
        Err(_) => panic!(
            "LIMINE_PATH environment variable not set. Please set it to the Limine directory."
        ),
    };

    // We will create an ISO file for our OS
    // First we create a folder which will be used to generate the ISO
    // We will use symlinks instead of copying to avoid unnecessary disk space used
    let iso_dir = out_dir.join("iso_root");
    create_dir_all(&iso_dir).unwrap();

    // Generate limine.conf, optionally with a test_suite cmdline for filtered runs.
    let test_suite = if env::var("CARGO_FEATURE_TEST_MEM").is_ok() {
        Some("mem")
    } else if env::var("CARGO_FEATURE_TEST_TIME").is_ok() {
        Some("time")
    } else if env::var("CARGO_FEATURE_TEST_INTERRUPTS").is_ok() {
        Some("interrupts")
    } else if env::var("CARGO_FEATURE_TEST_GRAPHICS").is_ok() {
        Some("graphics")
    } else if env::var("CARGO_FEATURE_TEST_USERMODE").is_ok() {
        Some("usermode")
    } else if env::var("CARGO_FEATURE_TEST_KEYBOARD").is_ok() {
        Some("keyboard")
    } else if env::var("CARGO_FEATURE_TEST_IPC").is_ok() {
        Some("ipc")
    } else if env::var("CARGO_FEATURE_TEST_DISPLAY").is_ok() {
        Some("display")
    } else if env::var("CARGO_FEATURE_TEST_SCHEDULER").is_ok() {
        Some("scheduler")
    } else if env::var("CARGO_FEATURE_TEST_ELF").is_ok() {
        Some("elf")
    } else if env::var("CARGO_FEATURE_TEST_SCHED").is_ok() {
        Some("sched")
    } else if env::var("CARGO_FEATURE_TEST_SCHED_NOELF").is_ok() {
        Some("sched-noelf")
    } else {
        None
    };

    let cmdline_line = match test_suite {
        Some(suite) => format!("    cmdline: test_suite={suite}\n"),
        None => String::new(),
    };

    let limine_conf_content = format!(
        "TIMEOUT 0\nDEFAULT_ENTRY 0\n\n/Bos\n    protocol: limine\n    kernel_path: boot():/kernel\n{cmdline_line}"
    );
    let limine_conf = iso_dir.join("limine.conf");
    std::fs::write(&limine_conf, limine_conf_content).unwrap();

    let boot_dir = iso_dir.join("boot");
    create_dir_all(&boot_dir).unwrap();

    // If the 'kernel_test' feature is enabled run the tests project bin.
    let kernel_executable_file = if env::var("CARGO_FEATURE_KERNEL_TEST").is_ok() {
        env::var("CARGO_BIN_FILE_TESTS")
            .expect("tests bin not built")
    } else {
        env::var("CARGO_BIN_FILE_KERNEL")
            .expect("kernel bin not built")
    };


    // Symlink the kernel binary to `kernel`
    let kernel_dest = iso_dir.join("kernel");
    ensure_symlink(&kernel_executable_file, &kernel_dest).unwrap();

    // Init Task
    let init_task_executable_file = env::var("CARGO_BIN_FILE_INIT_TASK").unwrap();
    ensure_symlink(init_task_executable_file, iso_dir.join("init_task")).unwrap();

    // Display Server
    let display_server_executable_file = env::var("CARGO_BIN_FILE_DISPLAY_SERVER").unwrap();
    ensure_symlink(display_server_executable_file, iso_dir.join("display_server")).unwrap();

    // User Land: Bouncing Cube 1
    let bouncing_cube_1_executable_file = env::var("CARGO_BIN_FILE_USER_LAND_BOUNCING_CUBE_1")
        .or_else(|_| env::var("CARGO_BIN_FILE_USER_LAND_bouncing_cube_1"))
        .expect("bouncing_cube_1 binary not found");
    ensure_symlink(bouncing_cube_1_executable_file, iso_dir.join("bouncing_cube_1")).unwrap();

    // User Land: Bouncing Cube 2
    let bouncing_cube_2_executable_file = env::var("CARGO_BIN_FILE_USER_LAND_BOUNCING_CUBE_2")
        .or_else(|_| env::var("CARGO_BIN_FILE_USER_LAND_bouncing_cube_2"))
        .expect("bouncing_cube_2 binary not found");
    ensure_symlink(bouncing_cube_2_executable_file, iso_dir.join("bouncing_cube_2")).unwrap();

    // Userspace integration test binary (only included when --features userspace_test)
    if env::var("CARGO_FEATURE_USERSPACE_TEST").is_ok() {
        let utest = env::var("CARGO_BIN_FILE_UTEST").expect("utest binary not built");
        ensure_symlink(utest, iso_dir.join("utest")).unwrap();
    }

    // Copy files from the Limine packaeg into `boot/limine`
    let out_limine_dir = boot_dir.join("limine");
    create_dir_all(&out_limine_dir).unwrap();
    for path in [
        "limine-bios.sys",
        "limine-bios-cd.bin",
        "limine-uefi-cd.bin",
    ] {
        let from = limine_dir.join(path);
        let to = out_limine_dir.join(path);
        ensure_symlink(from, to).unwrap();
    }

    // EFI/BOOT/BOOTX64.EFI is the executable loaded by UEFI firmware
    // We will also copy BOOTIA32.EFI because xorisso will complain if it's not there
    let efi_boot_dir = iso_dir.join("EFI/BOOT");
    create_dir_all(&efi_boot_dir).unwrap();
    for efi_file in ["BOOTX64.EFI", "BOOTIA32.EFI"] {
        ensure_symlink(limine_dir.join(efi_file), efi_boot_dir.join(efi_file)).unwrap();
    }

    // FS Server
    let fs_server_executable_file = env::var("CARGO_BIN_FILE_FS_SERVER").unwrap();
    ensure_symlink(fs_server_executable_file, iso_dir.join("fs_server")).unwrap();

    // ── FAT32 disk image ──────────────────────────────────────────────────────
    // Create a 64 MB raw FAT32 image (no MBR) at out_dir/disk.img.
    // Populate it with the app binaries so the fs_server can load them.
    let disk_img = out_dir.join("disk.img");
    create_fat32_disk_image(&disk_img);
    println!("cargo:rustc-env=DISK_IMG={}", disk_img.display());

    // Symlink the out dir so we get a constant path to it
    ensure_symlink(&out_dir, runner_dir.join("out_dir")).unwrap();

    // We'll call the output iso `os.iso`
    let output_iso = out_dir.join("os.iso");
    // This command creates an ISO file from our `iso_root` folder.
    // Symlinks will be read (the contents will be copied into the ISO file)
    let status = std::process::Command::new("xorriso")
        .arg("-as")
        .arg("mkisofs")
        .arg("--follow-links")
        .arg("-b")
        .arg(
            out_limine_dir
                .join("limine-bios-cd.bin")
                .strip_prefix(&iso_dir)
                .unwrap(),
        )
        .arg("-no-emul-boot")
        .arg("-boot-load-size")
        .arg("4")
        .arg("-boot-info-table")
        .arg("--efi-boot")
        .arg(
            out_limine_dir
                .join("limine-uefi-cd.bin")
                .strip_prefix(&iso_dir)
                .unwrap(),
        )
        .arg("-efi-boot-part")
        .arg("--efi-boot-image")
        .arg("--protective-msdos-label")
        .arg(iso_dir)
        .arg("-o")
        .arg(&output_iso)
        .stderr(Stdio::inherit())
        .stdout(Stdio::inherit())
        .status()
        .unwrap();
    assert!(status.success());

    // This is needed to create a hybrid ISO that boots on both BIOS and UEFI. See https://github.com/limine-bootloader/limine/blob/v9.x/USAGE.md#biosuefi-hybrid-iso-creation
    let status = std::process::Command::new("limine")
        .arg("bios-install")
        .arg(&output_iso)
        .stderr(Stdio::inherit())
        .stdout(Stdio::inherit())
        .status()
        .unwrap();
    assert!(status.success());

    let output_iso = output_iso.display();
    println!("cargo:rustc-env=ISO={output_iso}");
}

pub fn ensure_symlink<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> io::Result<()> {
    match remove_file(&link) {
        Ok(()) => Ok(()),
        Err(error) => match error.kind() {
            ErrorKind::NotFound => Ok(()),
            _ => Err(error),
        },
    }?;
    symlink(original, link)?;
    Ok(())
}

fn check_command_exists(cmd: &str) {
    if std::process::Command::new(cmd)
        .arg("--version")
        .output()
        .is_err()
    {
        panic!("Command '{}' not found. Please install it.", cmd);
    }
}

/// Create a 64 MB raw FAT32 disk image and populate it with app binaries.
fn create_fat32_disk_image(path: &PathBuf) {
    const DISK_SIZE: u64 = 64 * 1024 * 1024; // 64 MB

    // Allocate a buffer for the whole disk
    let mut disk: Cursor<Vec<u8>> = Cursor::new(vec![0u8; DISK_SIZE as usize]);

    // Format as FAT32
    fatfs::format_volume(
        &mut disk,
        fatfs::FormatVolumeOptions::new()
            .volume_label(*b"BOS_APPS   "),
    )
    .expect("fatfs: format_volume failed");

    // Open the filesystem and populate it
    disk.seek(SeekFrom::Start(0)).unwrap();
    {
        let fs = fatfs::FileSystem::new(&mut disk, fatfs::FsOptions::new())
            .expect("fatfs: FileSystem::new failed");
        let root = fs.root_dir();

        // Write each app binary that exists as a build artefact.
        // We add every user-land binary that is available; others are silently skipped.
        let apps: &[(&str, &str)] = &[
            ("BOUNCING_CUBE_1", "CUBE1.ELF"),
            ("BOUNCING_CUBE_2", "CUBE2.ELF"),
        ];
        for (env_suffixes, fat_name) in apps {
            // Try a few env var naming conventions Cargo uses for artifact bins
            let candidates = [
                format!("CARGO_BIN_FILE_USER_LAND_{env_suffixes}"),
                format!("CARGO_BIN_FILE_USER_LAND_{}", env_suffixes.to_lowercase()),
            ];
            let bin_path = candidates.iter().find_map(|e| std::env::var(e).ok());
            if let Some(p) = bin_path {
                match std::fs::read(&p) {
                    Ok(data) => {
                        let mut f = root.create_file(fat_name)
                            .expect("fatfs: create_file failed");
                        f.truncate().unwrap();
                        f.write_all(&data).unwrap();
                    }
                    Err(e) => eprintln!("build.rs: skipping {fat_name}: {e}"),
                }
            }
        }
    }

    // Write the image to disk
    let buf = disk.into_inner();
    std::fs::write(path, &buf).expect("build.rs: failed to write disk.img");
    println!("build.rs: wrote {} MB disk image to {}", DISK_SIZE / 1_048_576, path.display());
}
