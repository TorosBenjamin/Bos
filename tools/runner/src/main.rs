use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::{env, process};

/// Serve a static HTML page over HTTP/1.0 on 127.0.0.1:8000.
/// The guest reaches this via SLIRP at 10.0.2.2:8000.
fn spawn_stub_http_server() {
    std::thread::spawn(|| {
        let Ok(listener) = TcpListener::bind("127.0.0.1:8000") else { return };
        loop {
            let Ok((mut stream, _)) = listener.accept() else { continue };
            std::thread::spawn(move || {
                // Read (and discard) the HTTP request.
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);

                const BODY: &[u8] = b"<html><head><title>Bos OS</title></head><body>\
<h1>Bos OS</h1>\
<p>You are browsing the web from within a custom operating system!</p>\
<ul>\
<li>App: <b>boser</b> (Bos text browser)</li>\
<li>HTTP client: <b>http_client</b> (no_std, HTTP/1.0)</li>\
<li>TCP/IP stack: <b>smoltcp</b> inside <b>net_server</b></li>\
<li>NIC driver: <b>e1000</b> (Intel 82540EM, DMA ring)</li>\
<li>Networking: <b>QEMU user-mode (SLIRP)</b></li>\
</ul>\
<p>This page is served by the runner process on the host machine.</p>\
</body></html>";
                let header = format!(
                    "HTTP/1.0 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    BODY.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(BODY);
            });
        }
    });
}

fn main() {
    spawn_stub_http_server();
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

    // Attach the FAT32 disk image as an IDE drive (secondary to the CD-ROM)
    let disk_img = env!("DISK_IMG");
    qemu.arg("-drive").arg(format!(
        "file={disk_img},if=ide,format=raw,media=disk"
    ));

    qemu.arg("-device").arg("e1000,netdev=net0");
    qemu.arg("-netdev").arg("user,id=net0");

    let exit_status = qemu.status().expect("Failed to run QEMU");
    process::exit(exit_status.code().unwrap_or(1));
}