//! Stages the `ato` CLI sidecar for the Tauri bundle.
//!
//! tauri-build requires every `bundle.externalBin` entry to exist at compile
//! time under the name `<entry>-<target-triple>`, copies it into the target
//! dir, and the bundler then places it next to the shell executable inside the
//! .app — exactly the sibling the shell's release resolver expects. This
//! script materializes `bin/ato-<target-triple>`:
//!
//! - `ATO_DESKTOP_ATO_STAGE` points at an explicitly staged binary;
//! - otherwise the root workspace build (`../../target/<profile>/ato`) is used;
//! - release builds fail hard when neither exists — a release bundle without a
//!   real `ato` sidecar must never be produced;
//! - debug builds fall back to a placeholder that delegates to `ato` on PATH,
//!   so plain `cargo build` / `cargo test` keep working without a staged CLI.

use std::fs;
use std::path::{Path, PathBuf};

const STAGE_ENV: &str = "ATO_DESKTOP_ATO_STAGE";

fn main() {
    println!("cargo:rerun-if-env-changed={STAGE_ENV}");
    let manifest = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by Cargo"),
    );
    let target = std::env::var("TARGET").expect("TARGET is set by Cargo");
    let profile = std::env::var("PROFILE").expect("PROFILE is set by Cargo");
    let bin_dir = manifest.join("bin");
    fs::create_dir_all(&bin_dir).expect("create sidecar directory");
    let destination = bin_dir.join(format!("ato-{target}"));

    let root_build = manifest.join("../../target").join(&profile).join("ato");
    println!("cargo:rerun-if-changed={}", root_build.display());

    let staged = std::env::var_os(STAGE_ENV)
        .map(PathBuf::from)
        .or_else(|| root_build.is_file().then_some(root_build));
    match staged {
        Some(source) if source.is_file() => {
            fs::copy(&source, &destination).expect("stage ato sidecar");
        }
        _ if profile == "release" => panic!(
            "ato CLI sidecar is missing; run `cargo build --release -p ato-cli` first \
             (or set {STAGE_ENV} to an explicit binary)"
        ),
        _ => write_debug_placeholder(&destination),
    }
    tauri_build::build();
}

#[cfg(unix)]
fn write_debug_placeholder(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(
        path,
        "#!/bin/sh\n# Debug placeholder: delegate to the ato binary on PATH.\nexec ato \"$@\"\n",
    )
    .expect("write sidecar placeholder");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("make sidecar executable");
}

#[cfg(not(unix))]
fn write_debug_placeholder(path: &Path) {
    fs::write(
        path,
        "@echo off\necho ato sidecar is not staged for this build\nexit /b 1\n",
    )
    .expect("write sidecar placeholder");
}
