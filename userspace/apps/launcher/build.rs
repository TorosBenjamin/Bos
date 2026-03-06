fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() != "linux" {
        println!("cargo:rustc-link-arg-bins=-eentry_point");
    }
}
