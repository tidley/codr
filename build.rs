use std::process::Command;

fn main() {
    // Explicit Git watches disable Cargo's default package-wide invalidation.
    // Watch Rust, path dependencies and embedded helpers as well: otherwise a
    // dirty rebuild can reuse a cached clean flag, even after source is restored.
    for input in [
        "build.rs",
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        ".cargo",
        "src",
        "vendor/mdk",
        "crates",
        "connector.toml",
        "patches",
        "scripts",
        "systemd",
    ] {
        if std::path::Path::new(input).exists() {
            println!("cargo:rerun-if-changed={input}");
        }
    }
    println!("cargo:rerun-if-changed=.git/HEAD");
    // Rebuild identity when a branch advances, even if no Rust file changed.
    match Command::new("git")
        .args(["symbolic-ref", "-q", "HEAD"])
        .output()
    {
        Ok(output) if output.status.success() => {
            println!(
                "cargo:rerun-if-changed=.git/{}",
                String::from_utf8_lossy(&output.stdout).trim()
            );
        }
        _ => {}
    }
    println!("cargo:rerun-if-changed=.git/index");
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("Git source identity required");
    assert!(output.status.success(), "Git source identity unavailable");
    let revision = String::from_utf8(output.stdout).expect("Git revision is UTF-8");
    let revision = revision.trim();
    assert!(
        revision.len() == 40 && revision.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid Git revision"
    );
    let clean = Command::new("git")
        .args(["diff", "--quiet", "HEAD", "--"])
        .status()
        .expect("Git source status required")
        .success();
    println!("cargo:rustc-env=CODR_SOURCE_COMMIT={revision}");
    println!("cargo:rustc-env=CODR_SOURCE_DIRTY={}", !clean);
    println!(
        "cargo:rustc-env=CODR_BUILD_TARGET={}",
        std::env::var("TARGET").expect("Cargo target required")
    );
}
