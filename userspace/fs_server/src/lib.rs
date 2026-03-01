// Library crate root — used only for `cargo test -p fs_server` (host target).
// No_std when not testing (e.g. `cargo check --target x86_64-unknown-none`).
#![cfg_attr(not(test), no_std)]

pub mod fat32;
