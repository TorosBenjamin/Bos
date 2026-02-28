use std::fs;
use std::process::Command;
use std::{env, process};

fn main() {
    let ovmf_code = "/usr/share/OVMF/OVMF_CODE_4M.fd";
    let ovmf_vars_readonly = "/usr/share/OVMF/OVMF_VARS_4M.fd";

    // Create a local path for the vars file so we can write to it
    let out_dir = env::current_dir().unwrap().join("target");
    let local_vars = out_dir.join("OVMF_VARS_LOCAL.fd");

    // Copy the system vars file to our local target directory if it doesn't exist
    if !local_vars.exists() {
        fs::copy(ovmf_vars_readonly, &local_vars).expect("Failed to copy OVMF_VARS to local directory");
    }

    let number_of_cpus = 5;
    let mut qemu = Command::new("qemu-system-x86_64");

    qemu.arg("-enable-kvm");
    qemu.arg("-cdrom").arg(env!("ISO"));

    // Unit 0: The Code (Read-Only is fine)
    qemu.arg("-drive").arg(format!(
        "if=pflash,format=raw,unit=0,file={ovmf_code},readonly=on"
    ));

    // Unit 1: The local copy of Vars (Now we have write permission!)
    qemu.arg("-drive")
        .arg(format!("if=pflash,format=raw,unit=1,file={}", local_vars.display()));

    // ... rest of your SMP, Serial, and CPU arguments ...
    qemu.arg("--smp").arg(number_of_cpus.to_string());
    qemu.arg("--no-reboot");
    qemu.arg("-serial").arg("stdio");
    qemu.arg("-device").arg("isa-debug-exit,iobase=0xf4,iosize=0x04");
    qemu.arg("-cpu").arg("host");
    // qemu.arg("-display").arg("none");

    let exit_status = qemu.status().expect("Failed to run QEMU");
    process::exit(exit_status.code().unwrap_or(1));
}