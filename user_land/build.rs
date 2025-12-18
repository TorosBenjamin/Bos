fn main() {
    // Specify in the output ELF what the entry function is
    let entry_function = "entry_point";
    println!("cargo:rustc-link-arg=-e{entry_function}");
}