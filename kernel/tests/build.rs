use std::env;
use std::path::PathBuf;

fn main() {
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let kernel_core_dir = manifest_dir.parent().unwrap().join("core");

    let linker_file = kernel_core_dir.join(format!("linker-{}.ld", arch));

    println!("cargo:rustc-link-arg=-T{}", linker_file.display());
    println!("cargo:rerun-if-changed={}", linker_file.display());
}
