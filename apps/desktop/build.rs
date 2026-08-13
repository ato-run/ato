use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(
        env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR should be set by Cargo"),
    );

    // Embed the app icon into the Windows executable so Explorer,
    // the taskbar, and the Start Menu shortcut all show the Ato icon.
    #[cfg(windows)]
    embed_windows_icon(&manifest_dir);

    // ato-onboarding system capsule (Vite + React)
    check_onboarding_dist(&manifest_dir);

    // ato-dock system capsule (Vite + React)
    check_dock_dist(&manifest_dir);

    // ato-start system capsule (Astro)
    check_start_dist(&manifest_dir);

    // ato-store system capsule (Astro desktop static build)
    check_store_dist(&manifest_dir);

    // ato-cli + nacelle helpers — rebuild in lockstep with ato-desktop so
    // `cargo run --bin ato-desktop` never picks up a stale binary.
    rebuild_helpers(&manifest_dir);
}

/// Embeds `assets/AppIcon.ico` into the Windows executable using an RC
/// resource file so Explorer, the taskbar, and the Start Menu shortcut
/// all display the Ato icon without requiring an explicit `Icon` entry in
/// the WiX shortcut element (Windows inherits the first icon resource from
/// the exe automatically).
///
/// Only active when the build host is Windows (`#[cfg(windows)]`).
/// The `embed-resource` crate is declared under
/// `[target.'cfg(windows)'.build-dependencies]` so it is not even resolved
/// on macOS/Linux builds.
#[cfg(windows)]
fn embed_windows_icon(manifest_dir: &Path) {
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("assets/AppIcon.ico").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("assets/windows/ato-desktop.rc").display()
    );
    let _compilation =
        embed_resource::compile("assets/windows/ato-desktop.rc", embed_resource::NONE);
}

/// Keep the `ato` and `nacelle` helper binaries in sync with the current
/// source tree.
///
/// In the monorepo dev workflow, `cargo run --bin ato-desktop` only builds
/// `ato-desktop` itself — `ato-cli` and `nacelle` live in the root workspace
/// (which excludes `ato-desktop` to dodge crates.io packaging issues), so
/// they would otherwise stay frozen at whatever version was last built
/// manually. That lets old binaries sneak in as the desktop's helpers.
///
/// This build step:
///   1. Locates the root workspace (`manifest_dir/../../`) and verifies it
///      declares both helper crates.
///   2. Shells out to `cargo build -p ato-cli -p nacelle` (release-gated by
///      the current `PROFILE`) against that workspace.
///   3. Emits `ATO_DESKTOP_DEV_HELPER_TARGET=<root>/target` so the runtime
///      resolver can prefer the freshly-built helpers over PATH lookups.
///
/// Opt out with `ATO_DESKTOP_SKIP_HELPER_BUILD=1` (CI/release pipelines
/// that pre-stage helpers, or when iterating on `ato-desktop` alone).
fn rebuild_helpers(manifest_dir: &Path) {
    println!("cargo:rerun-if-env-changed=ATO_DESKTOP_SKIP_HELPER_BUILD");

    let Some(workspace_root) = manifest_dir.parent().and_then(|p| p.parent()) else {
        println!(
            "cargo:warning=could not derive root workspace from {} — skipping helper rebuild",
            manifest_dir.display()
        );
        return;
    };

    let root_cargo = workspace_root.join("Cargo.toml");
    if !root_cargo.is_file() {
        // Source distribution without the monorepo (e.g. vendored crate
        // tarball). Runtime resolver will fall back to PATH.
        return;
    }

    let manifest = std::fs::read_to_string(&root_cargo).unwrap_or_default();
    if !manifest.contains("crates/cli") || !manifest.contains("crates/nacelle") {
        // Root Cargo.toml exists but doesn't own the helpers — bail out so
        // we never poke an unrelated workspace.
        return;
    }

    // Watch helper crate dirs so cargo re-runs build.rs when their sources
    // change. These watches are additive: existing rerun-if-changed entries
    // for asset dist dirs above still apply.
    for dir in ["../cli", "../nacelle", "../capsule", "../protocol"] {
        println!("cargo:rerun-if-changed={dir}");
    }

    let helper_target = workspace_root.join("target");
    println!(
        "cargo:rustc-env=ATO_DESKTOP_DEV_HELPER_TARGET={}",
        helper_target.display()
    );

    if env_truthy("ATO_DESKTOP_SKIP_HELPER_BUILD") {
        println!(
            "cargo:warning=ATO_DESKTOP_SKIP_HELPER_BUILD=1 set; not rebuilding ato-cli/nacelle"
        );
        return;
    }

    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".into());
    let mut args = vec!["build", "-p", "cli", "-p", "nacelle"];
    if profile == "release" {
        args.push("--release");
    }

    println!(
        "cargo:warning=rebuilding ato-cli + nacelle helpers ({profile}) in {}",
        workspace_root.display()
    );

    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = Command::new(&cargo)
        .args(&args)
        .current_dir(workspace_root)
        // Don't inherit ato-desktop's CARGO_TARGET_DIR — let the root
        // workspace use its own `target/` so the resolver can find the
        // produced binaries at <root>/target/{profile}/{ato,nacelle}.
        .env_remove("CARGO_TARGET_DIR")
        .status();

    match status {
        Ok(status) if status.success() => {}
        Ok(status) => {
            panic!(
                "helper rebuild (cargo build -p cli -p nacelle) failed with status {status} in {}",
                workspace_root.display()
            );
        }
        Err(error) => {
            panic!(
                "failed to invoke `{cargo}` for helper rebuild in {}: {error}",
                workspace_root.display()
            );
        }
    }
}

fn check_onboarding_dist(manifest_dir: &Path) {
    let capsule_dir = manifest_dir
        .join("assets")
        .join("system")
        .join("ato-onboarding");
    let dist_dir = capsule_dir.join("dist");
    let entrypoint = dist_dir.join("index.html");

    println!("cargo:rerun-if-changed={}", dist_dir.display());
    println!("cargo:rerun-if-env-changed=ATO_DESKTOP_SKIP_ONBOARDING_BUILD");

    if entrypoint.exists() {
        if env_truthy("ATO_DESKTOP_SKIP_ONBOARDING_BUILD") {
            println!(
                "cargo:warning=ATO_DESKTOP_SKIP_ONBOARDING_BUILD=1 set; using existing onboarding dist at {}",
                dist_dir.display()
            );
        }
        return;
    }

    if env_truthy("ATO_DESKTOP_SKIP_ONBOARDING_BUILD") {
        println!(
            "cargo:warning=ATO_DESKTOP_SKIP_ONBOARDING_BUILD=1 set; onboarding dist check skipped"
        );
        return;
    }

    if !capsule_dir.join("node_modules").exists() {
        run_command(
            "npm",
            &["install"],
            &capsule_dir,
            "ato-onboarding npm install",
        );
    }
    run_command(
        "npm",
        &["run", "build"],
        &capsule_dir,
        "ato-onboarding vite build",
    );
    if entrypoint.exists() {
        return;
    }

    panic!(
        "ato-onboarding dist/index.html missing at {}. Set ATO_DESKTOP_SKIP_ONBOARDING_BUILD=1 to skip or run `npm run build` in assets/system/ato-onboarding/.",
        dist_dir.display()
    );
}

fn check_dock_dist(manifest_dir: &Path) {
    let dock_dir = manifest_dir.join("assets").join("system").join("ato-dock");
    let dist_dir = dock_dir.join("dist");

    println!(
        "cargo:rerun-if-changed={}",
        dock_dir.join("App.jsx").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        dock_dir.join("index.html").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        dock_dir.join("capsule.toml").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        dock_dir.join("package.json").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        dock_dir.join("package-lock.json").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        dock_dir.join("vite.config.js").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        dock_dir.join("src").join("main.jsx").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        dock_dir.join("src").join("index.css").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        dock_dir.join("src").join("bridge.js").display()
    );
    println!("cargo:rerun-if-env-changed=ATO_DESKTOP_SKIP_DOCK_BUILD");

    let entrypoint = dist_dir.join("index.html");

    if entrypoint.exists() {
        if env_truthy("ATO_DESKTOP_SKIP_DOCK_BUILD") {
            println!(
                "cargo:warning=ATO_DESKTOP_SKIP_DOCK_BUILD=1 set; using existing dock dist at {}",
                dist_dir.display()
            );
        }
        return;
    }

    if env_truthy("ATO_DESKTOP_SKIP_DOCK_BUILD") {
        println!("cargo:warning=ATO_DESKTOP_SKIP_DOCK_BUILD=1 set; dock dist check skipped");
        return;
    }

    if !dock_dir.join("node_modules").exists() {
        run_command(
            "npm",
            &["install"],
            &dock_dir,
            "ato-dock dependency install",
        );
    }

    run_command("npm", &["run", "build"], &dock_dir, "ato-dock build");

    if entrypoint.exists() {
        return;
    }

    panic!(
        "ato-dock dist/index.html missing at {} after build. Run `npm install && npm run build` in assets/system/ato-dock/.",
        dist_dir.display()
    );
}

fn run_command(binary: &str, args: &[&str], cwd: &PathBuf, label: &str) {
    let status = Command::new(binary).args(args).current_dir(cwd).status();
    match status {
        Ok(status) if status.success() => {}
        Ok(status) => {
            panic!(
                "{} failed with status {} in {}",
                label,
                status,
                cwd.display()
            );
        }
        Err(error) => {
            panic!(
                "failed to execute `{}` for {} in {}: {}",
                binary,
                label,
                cwd.display(),
                error
            );
        }
    }
}

fn check_start_dist(manifest_dir: &Path) {
    let capsule_dir = manifest_dir.join("assets").join("system").join("ato-start");
    let dist_dir = capsule_dir.join("dist");
    let entrypoint = dist_dir.join("index.html");

    println!("cargo:rerun-if-changed={}", dist_dir.display());
    println!("cargo:rerun-if-env-changed=ATO_DESKTOP_SKIP_START_BUILD");

    if entrypoint.exists() {
        if env_truthy("ATO_DESKTOP_SKIP_START_BUILD") {
            println!(
                "cargo:warning=ATO_DESKTOP_SKIP_START_BUILD=1 set; using existing start dist at {}",
                dist_dir.display()
            );
        }
        return;
    }

    if env_truthy("ATO_DESKTOP_SKIP_START_BUILD") {
        println!("cargo:warning=ATO_DESKTOP_SKIP_START_BUILD=1 set; start dist check skipped");
        return;
    }

    if !capsule_dir.join("node_modules").exists() {
        run_command("npm", &["install"], &capsule_dir, "ato-start npm install");
    }
    run_command(
        "npm",
        &["run", "build"],
        &capsule_dir,
        "ato-start astro build",
    );
    if entrypoint.exists() {
        return;
    }

    panic!(
        "ato-start dist/index.html missing at {}. Set ATO_DESKTOP_SKIP_START_BUILD=1 to skip or run `npm run build` in assets/system/ato-start/.",
        dist_dir.display()
    );
}

fn check_store_dist(manifest_dir: &Path) {
    let dist_dir = manifest_dir
        .join("assets")
        .join("system")
        .join("ato-store")
        .join("dist");
    let entrypoint = dist_dir.join("index.html");

    println!("cargo:rerun-if-changed={}", dist_dir.display());
    println!("cargo:rerun-if-env-changed=ATO_DESKTOP_SKIP_STORE_BUILD");

    if entrypoint.exists() {
        if env_truthy("ATO_DESKTOP_SKIP_STORE_BUILD") {
            println!(
                "cargo:warning=ATO_DESKTOP_SKIP_STORE_BUILD=1 set; using existing store dist at {}",
                dist_dir.display()
            );
        }
        return;
    }

    if env_truthy("ATO_DESKTOP_SKIP_STORE_BUILD") {
        println!("cargo:warning=ATO_DESKTOP_SKIP_STORE_BUILD=1 set; store dist check skipped");
        return;
    }

    panic!(
        "ato-store dist/index.html missing at {}. Set ATO_DESKTOP_SKIP_STORE_BUILD=1 to skip or build ato-web with `pnpm run build:desktop-store` and copy dist to assets/system/ato-store/dist/.",
        entrypoint.display()
    );
}

fn env_truthy(key: &str) -> bool {
    match env::var(key) {
        Ok(value) => {
            let trimmed = value.trim();
            !trimmed.is_empty() && !matches!(trimmed, "0" | "false" | "off" | "no")
        }
        Err(_) => false,
    }
}
