//! Build Swift HUD into target/{profile}/ — same as clipd-daemon / clipd-gui so
//! `cargo build -p clipd-ui` still produces clipd-hud for Clipd.app packaging.

use std::path::PathBuf;
use std::process::Command;

fn hud_binary_path() -> PathBuf {
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".into());
    let target_root = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../target")
        });
    target_root.join(&profile).join("clipd-hud")
}

fn main() {
    println!("cargo:rerun-if-changed=../clipd-hud/clipd-hud.swift");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "macos" {
        return;
    }

    let dest = hud_binary_path();
    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set"),
    );
    let swift_src = manifest_dir.join("../clipd-hud/clipd-hud.swift");
    if !swift_src.is_file() {
        println!(
            "cargo:warning=clipd-hud: missing {} — HUD overlay will not be available",
            swift_src.display()
        );
        return;
    }

    let status = Command::new("swiftc")
        .arg("-O")
        .arg("-o")
        .arg(&dest)
        .arg(&swift_src)
        .args(["-framework", "Cocoa"])
        .status();

    match status {
        Ok(s) if s.success() => {
            let _ = Command::new("codesign")
                .args(["--force", "--sign", "-"])
                .arg(&dest)
                .status();
        }
        Ok(_) => println!(
            "cargo:warning=clipd-hud: swiftc failed — install Xcode Command Line Tools for the overlay HUD"
        ),
        Err(_) => println!(
            "cargo:warning=clipd-hud: swiftc not found — install Xcode Command Line Tools for the overlay HUD"
        ),
    }
}
