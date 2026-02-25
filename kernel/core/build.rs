use std::path::PathBuf;

fn main() {
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let linker_file = PathBuf::from(dir).join(format!("linker-{arch}.ld"));
    let linker_file = linker_file.to_str().unwrap();

    println!("cargo:rustc-link-arg=-T{linker_file}");
    println!("cargo:rerun-if-changed={linker_file}");
}
