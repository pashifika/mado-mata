use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;

fn hash_path(hash: &mut Sha256, path: &Path) {
    println!("cargo:rerun-if-changed={}", path.display());
    if path.is_dir() {
        let mut paths: Vec<_> = std::fs::read_dir(path)
            .expect("read application directory")
            .map(|entry| entry.expect("read application entry").path())
            .collect();
        paths.sort();
        for path in paths {
            hash_path(hash, &path);
        }
    } else {
        let name = path.to_string_lossy().replace('\\', "/");
        let bytes = std::fs::read(path).expect("read application input");
        hash.update((name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
}

fn main() {
    let rustc = std::env::var_os("RUSTC").expect("Cargo supplies RUSTC");
    let output = Command::new(rustc)
        .arg("--version")
        .output()
        .expect("query selected rustc");
    assert!(
        output.status.success(),
        "selected rustc did not report its version"
    );
    let version = String::from_utf8(output.stdout).expect("rustc version is UTF-8");
    println!("cargo:rustc-env=COMPARISON_RUSTC={}", version.trim());
    println!(
        "cargo:rustc-env=COMPARISON_TARGET={}",
        std::env::var("TARGET").expect("Cargo supplies TARGET")
    );
    println!(
        "cargo:rustc-env=COMPARISON_PROFILE={}",
        std::env::var("PROFILE").expect("Cargo supplies PROFILE")
    );
    let mut hash = Sha256::new();
    for path in [
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        "build.rs",
        "src",
        "fixtures",
        "compiler/compile.mjs",
        "compiler/worker.mjs",
        "compiler/sdk.mjs",
        "compiler/package.json",
        "compiler/package-lock.json",
    ] {
        hash_path(&mut hash, Path::new(path));
    }
    println!(
        "cargo:rustc-env=COMPARISON_SOURCE_SHA256={:x}",
        hash.finalize()
    );
    println!("cargo:rerun-if-changed=Cargo.lock");
    println!("cargo:rerun-if-changed=build.rs");
}
