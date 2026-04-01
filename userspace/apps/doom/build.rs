use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let doom_dir = manifest.join("doomgeneric/doomgeneric");
    let include_dir = manifest.join("include");

    // Platform-specific files to exclude
    let exclude = [
        "doomgeneric_sdl.c",
        "doomgeneric_xlib.c",
        "doomgeneric_win.c",
        "doomgeneric_allegro.c",
        "doomgeneric_emscripten.c",
        "doomgeneric_linuxvt.c",
        "doomgeneric_soso.c",
        "doomgeneric_sosox.c",
        "i_sdlmusic.c",
        "i_sdlsound.c",
        "i_allegromusic.c",
        "i_allegrosound.c",
        "icon.c",
    ];

    let mut build = cc::Build::new();
    build
        .compiler("clang")
        .flag("--target=x86_64-unknown-none-elf")
        .flag("-ffreestanding")
        .flag("-nostdlib")
        .flag("-mno-red-zone")
        .flag("-fno-stack-protector")
        .flag("-fno-exceptions")
        .flag("-fno-builtin")
        .flag("-fno-pie")
        .flag("-w")               // suppress all warnings (doom code is old)
        .include(&include_dir)
        .include(&doom_dir)
        .define("NORMALUNIX", None)
        .define("LINUX", None);

    // Add all doom .c files except excluded ones
    for entry in std::fs::read_dir(&doom_dir).expect("can't read doomgeneric dir") {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("c") {
            continue;
        }
        let name = path.file_name().unwrap().to_str().unwrap();
        if exclude.contains(&name) {
            continue;
        }
        build.file(&path);
        println!("cargo:rerun-if-changed={}", path.display());
    }

    // Also compile our bos libc helpers (printf, etc.)
    let libc_c = manifest.join("bos_libc.c");
    build.file(&libc_c);
    println!("cargo:rerun-if-changed={}", libc_c.display());

    build.compile("doomgeneric");

    println!("cargo:rerun-if-changed={}", include_dir.display());
    println!("cargo:rerun-if-changed=build.rs");
}
