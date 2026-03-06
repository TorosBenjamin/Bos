fn main() {
    // Only set the custom entry point for the binary target.
    // Using rustc-link-arg (without -bins) would also apply to the lib test binary,
    // which has no `entry_point` symbol, causing the linker to set entry = 0x0 → SIGSEGV.
    println!("cargo:rustc-link-arg-bins=-eentry_point");
}
