use std::path::{Path, PathBuf};

fn main() {
    // bootstrap.py lives at the repo root (it runs inside Blender and stays Python).
    // Copy it into OUT_DIR so the crate can embed it with include_str!.
    let src = Path::new("../../bootstrap.py");
    println!("cargo:rerun-if-changed=../../bootstrap.py");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("bootstrap.py");
    std::fs::copy(src, &out).unwrap_or_else(|e| panic!("copy {} -> {}: {e}", src.display(), out.display()));
    tauri_build::build()
}
