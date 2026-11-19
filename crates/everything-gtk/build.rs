use std::path::Path;

fn main() {
    let out = std::env::var("OUT_DIR").expect("OUT_DIR");
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../dist/everythingd");
    let dst = Path::new(&out).join("everythingd");
    if src.is_file() {
        std::fs::copy(&src, &dst).expect("copy embedded daemon");
        println!("cargo:rerun-if-changed={}", src.display());
    } else {
        std::fs::write(&dst, b"").expect("create empty embedded daemon");
        println!("cargo:warning=everythingd not found in dist; GUI will rely on external daemon");
    }
}
