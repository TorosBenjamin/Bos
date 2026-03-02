fn main() {
    // Set the ELF entry point to `entry_point` on bare-metal targets.
    // On Linux the standard `main` entry is used instead.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "linux" {
        println!("cargo:rustc-link-arg-bins=-eentry_point");
    }
}
