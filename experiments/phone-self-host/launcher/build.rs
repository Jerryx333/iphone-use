use sha2::{Digest, Sha256};
fn main() {
    let mut hash = Sha256::new();
    for path in [
        "src/lib.rs",
        "src/main.rs",
        "src/image.rs",
        "Cargo.toml",
        "Cargo.lock",
        "../patches/idevice-rsd-xctest.patch",
        "../.deps/idevice/idevice/src/services/dvt/xctest/mod.rs",
    ] {
        println!("cargo:rerun-if-changed={path}");
        hash.update(std::fs::read(path).expect("native host build input missing"));
    }
    println!("cargo:rustc-env=IPU_HOST_BUILD_ID={:x}", hash.finalize());
}
