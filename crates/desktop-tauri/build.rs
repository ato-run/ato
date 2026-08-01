use std::path::{Path, PathBuf};
use std::process::Command;

const SKIP_BUILD_ENV: &str = "ATO_DESKTOP_SKIP_FRONTEND_BUILD";
const PWA_DIR_ENV: &str = "ATO_DESKTOP_PWA_DIR";
const NPM_ENV: &str = "ATO_DESKTOP_NPM";

fn main() {
    println!("cargo:rerun-if-env-changed={SKIP_BUILD_ENV}");
    println!("cargo:rerun-if-env-changed={PWA_DIR_ENV}");
    println!("cargo:rerun-if-env-changed={NPM_ENV}");

    let manifest_dir = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by Cargo"),
    );
    let frontend_dir = manifest_dir.join("frontend");

    if std::env::var_os(SKIP_BUILD_ENV).as_deref() != Some(std::ffi::OsStr::new("1")) {
        let pwa_dir = std::env::var_os(PWA_DIR_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|| manifest_dir.join("../../../ato-pwa"));
        build_desktop_frontend(&pwa_dir);
        sync_directory(&pwa_dir.join("dist-desktop"), &frontend_dir);
    } else if !frontend_dir.join("index.html").is_file() {
        panic!(
            "{SKIP_BUILD_ENV}=1 requires a prepared {}",
            frontend_dir.display()
        );
    }

    tauri_build::build();
}

fn build_desktop_frontend(pwa_dir: &Path) {
    let package_json = pwa_dir.join("package.json");
    if !package_json.is_file() {
        panic!(
            "ato-pwa was not found at {} (override with {PWA_DIR_ENV})",
            pwa_dir.display()
        );
    }

    for path in [
        package_json,
        pwa_dir.join("package-lock.json"),
        pwa_dir.join("desktop.html"),
        pwa_dir.join("vite.desktop.config.ts"),
        pwa_dir.join("src/desktop"),
    ] {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let npm = std::env::var_os(NPM_ENV).unwrap_or_else(|| "npm".into());
    if !pwa_dir.join("node_modules").is_dir() {
        let status = Command::new(&npm)
            .args(["ci", "--ignore-scripts"])
            .current_dir(pwa_dir)
            .status()
            .unwrap_or_else(|error| panic!("failed to execute {:?} ci: {error}", npm));
        if !status.success() {
            panic!("ato-pwa dependency install failed with {status}");
        }
    }
    let status = Command::new(&npm)
        .args(["run", "build:desktop"])
        .current_dir(pwa_dir)
        .status()
        .unwrap_or_else(|error| panic!("failed to execute {:?}: {error}", npm));
    if !status.success() {
        panic!("ato-pwa Desktop build failed with {status}");
    }
}

fn sync_directory(source: &Path, destination: &Path) {
    if !source.join("index.html").is_file() {
        panic!("Desktop frontend output is missing at {}", source.display());
    }
    if destination.exists() {
        std::fs::remove_dir_all(destination)
            .unwrap_or_else(|error| panic!("failed to clear {}: {error}", destination.display()));
    }
    copy_directory(source, destination);
}

fn copy_directory(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination)
        .unwrap_or_else(|error| panic!("failed to create {}: {error}", destination.display()));
    for entry in std::fs::read_dir(source)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", source.display()))
    {
        let entry = entry.expect("read Desktop frontend entry");
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if entry
            .file_type()
            .expect("read Desktop frontend file type")
            .is_dir()
        {
            copy_directory(&source_path, &destination_path);
        } else {
            std::fs::copy(&source_path, &destination_path).unwrap_or_else(|error| {
                panic!(
                    "failed to copy {} to {}: {error}",
                    source_path.display(),
                    destination_path.display()
                )
            });
        }
    }
}
