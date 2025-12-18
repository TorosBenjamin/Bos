use std::process::Command;
use std::{env, process};

fn main() {
    let ovmf_code = "/usr/share/edk2/x64/OVMF_CODE.4m.fd";
    let ovmf_vars = "/usr/share/edk2/x64/OVMF_VARS.4m.fd";
    let number_of_cpus = 10;

    let mut qemu = Command::new("qemu-system-x86_64");

    // ISO
    qemu.arg("-cdrom").arg(env!("ISO"));

    // UEFI firmware (two parts)
    qemu.arg("-drive").arg(format!(
        "if=pflash,format=raw,unit=0,file={ovmf_code},readonly=on"
    ));

    qemu.arg("-drive")
        .arg(format!("if=pflash,format=raw,unit=1,file={ovmf_vars}"));

    qemu.arg("--smp").arg(number_of_cpus.to_string());

    qemu.arg("--no-reboot");
    qemu.arg("-d").arg("int");

    // Pass any arguments
    env::args().skip(1).for_each(|arg| {
        qemu.arg(arg);
    });

    let exit_status = qemu.status().unwrap();
    process::exit(exit_status.code().unwrap_or(1));
}
