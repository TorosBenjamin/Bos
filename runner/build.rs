use std::fs::{create_dir_all, remove_file};
use std::io::ErrorKind;
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

    // Limine config will be in `limine.conf`
    let limine_conf = iso_dir.join("limine.conf");
    ensure_symlink(runner_dir.join("limine.conf"), limine_conf).unwrap();

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

    // User Land binaries - check all env vars to find the right names
    for (key, value) in env::vars() {
        if key.starts_with("CARGO_BIN_FILE") && key.contains("USER_LAND") {
            eprintln!("Found user_land env var: {} = {}", key, value);
        }
    }

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
