use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

const APP_NAME: &str = "Ato Desktop";
const APP_IDENTIFIER: &str = "run.ato.desktop";
const DEFAULT_TARGET: &str = "darwin-arm64";

fn main() -> Result<()> {
    let all: Vec<String> = std::env::args().skip(1).collect();

    let Some(cmd) = all.first().map(String::as_str) else {
        print_help();
        return Ok(());
    };

    match cmd {
        "frontend" => {
            let subcommand = all.get(1).map(String::as_str);
            let forwarded = normalize_passthrough_args(all[2..].to_vec());
            match subcommand {
                Some("build") => frontend_build(&forwarded),
                Some("dev") => frontend_dev(&forwarded),
                Some("help") | Some("--help") | Some("-h") | None => {
                    print_frontend_help();
                    Ok(())
                }
                Some(other) => bail!("unsupported frontend command: {}", other),
            }
        }
        "store" => {
            let forwarded = normalize_passthrough_args(all[1..].to_vec());
            let do_install = forwarded.iter().any(|a| a == "--install");
            store_build(do_install)
        }
        "bundle" => {
            let mut target = DEFAULT_TARGET.to_string();
            let mut sign = false;
            let mut do_notarize = false;
            let mut do_zip = false;
            let mut do_msi = false;
            let mut do_appimage = false;
            let mut i = 1;
            while let Some(arg) = all.get(i) {
                i += 1;
                match arg.as_str() {
                    "--target" => {
                        target = all
                            .get(i)
                            .context("--target requires a value such as darwin-arm64")?
                            .clone();
                        i += 1;
                    }
                    "--sign" => sign = true,
                    "--notarize" => do_notarize = true,
                    "--zip" => do_zip = true,
                    "--msi" => do_msi = true,
                    "--appimage" => do_appimage = true,
                    other => bail!("unsupported xtask argument: {}", other),
                }
            }
            // Dispatch by target family. Each platform has its own
            // staging layout — keeping them in distinct functions
            // makes the per-platform invariants (Helpers/ato vs
            // bin\ato.exe vs usr/bin/ato) easy to verify.
            //
            // macOS .zip via `ditto -c -k --keepParent` preserves the
            // codesign xattrs that hdiutil/.dmg lose; .dmg is also
            // quarantine-tainted when downloaded via Safari, so the
            // zip path is now the canonical install.sh delivery.
            match target.as_str() {
                "darwin-arm64" | "darwin-x86_64" => {
                    let bundle = bundle_macos_app(&target)?;
                    if sign {
                        codesign_bundle(&bundle)?;
                    }
                    if do_notarize {
                        notarize_bundle(&bundle)?;
                    }
                    if do_zip {
                        package_macos_zip(&bundle, &target)?;
                    }
                    Ok(())
                }
                "windows-x86_64" => {
                    let staging = bundle_windows_app(&target)?;
                    if do_msi {
                        package_msi(&staging, &target)?;
                    }
                    if do_zip {
                        package_windows_zip(&staging, &target)?;
                    }
                    Ok(())
                }
                "linux-x86_64" | "linux-arm64" => {
                    let staging = bundle_linux_app(&target)?;
                    if do_appimage {
                        package_appimage(&staging, &target)?;
                    }
                    Ok(())
                }
                other => bail!("unsupported bundle target: {}", other),
            }
        }
        "notarize" => {
            let bundle = all
                .get(1)
                .context("notarize requires a path to the .app bundle")?;
            notarize_bundle(Path::new(bundle))
        }
        "zip" => {
            let path = all
                .get(1)
                .context("zip requires a path to a .app bundle (macOS) or staging dir (Windows)")?;
            let target = Path::new(&path)
                .parent()
                .and_then(Path::file_name)
                .and_then(|s| s.to_str())
                .unwrap_or(DEFAULT_TARGET)
                .to_string();
            match target.as_str() {
                "darwin-arm64" | "darwin-x86_64" => package_macos_zip(Path::new(&path), &target),
                "windows-x86_64" => package_windows_zip(Path::new(&path), &target),
                other => bail!("unsupported zip target: {}", other),
            }
        }
        "msi" => {
            let staging = all
                .get(1)
                .context("msi requires a path to the staging directory")?;
            let target = Path::new(&staging)
                .parent()
                .and_then(Path::file_name)
                .and_then(|s| s.to_str())
                .unwrap_or("windows-x86_64")
                .to_string();
            package_msi(Path::new(&staging), &target)
        }
        "appimage" => {
            let staging = all
                .get(1)
                .context("appimage requires a path to the staging directory")?;
            let target = Path::new(&staging)
                .parent()
                .and_then(Path::file_name)
                .and_then(|s| s.to_str())
                .unwrap_or("linux-x86_64")
                .to_string();
            package_appimage(Path::new(&staging), &target)
        }
        other => bail!("unsupported xtask command: {}", other),
    }
}

fn print_help() {
    println!(
        "ato-desktop xtask\n\n\
         Commands:\n  \
                     frontend build [-- <vite-args...>]\n  \
                     frontend dev   [-- <vite-args...>]\n  \
                     store build [--install]\n  \
            bundle [--target TARGET] [--sign] [--notarize] [--zip] [--msi] [--appimage]\n  \
            notarize <bundle>     Submit an .app to Apple notary (no-op without APPLE_* env)\n  \
            zip      <path>       Wrap a .app bundle (macOS) or staging dir (Windows) in a .zip\n  \
            msi      <staging>    Wrap a Windows staging tree in an .msi via WiX (candle/light)\n  \
            appimage <staging>    Wrap a Linux staging tree in an .AppImage via appimagetool\n\n\
         Targets:\n  \
            darwin-arm64 (default), darwin-x86_64, windows-x86_64, linux-x86_64, linux-arm64\n\n\
         macOS code-signing modes (resolved at runtime):\n  \
            - if MAC_DEVELOPER_ID_NAME is set: real Developer ID (hardened runtime + entitlements)\n  \
            - else:                            ad-hoc (`codesign --sign -`) — v0.5 default\n\n\
         Windows: signtool integration is scaffolded but env-gated; v0.5 ships unsigned (L10).\n"
    );
}

fn store_build(do_install: bool) -> Result<()> {
    let paths = WorkspacePaths::discover()?;
    let store_root = &paths.store_root;

    if !store_root.join("node_modules").exists() || do_install {
        println!("Installing store dependencies…");
        let status = Command::new("pnpm")
            .args(["install", "--frozen-lockfile"])
            .current_dir(store_root)
            .status()
            .context("failed to run pnpm install in apps/ato-web")?;
        if !status.success() {
            bail!("pnpm install failed with status {}", status);
        }
    }

    println!("Building desktop-store…");
    let status = Command::new("pnpm")
        .args(["run", "build:desktop-store"])
        .current_dir(store_root)
        .status()
        .context("failed to run pnpm build:desktop-store in apps/ato-web")?;
    if !status.success() {
        bail!("pnpm build:desktop-store failed with status {}", status);
    }

    let src = &paths.store_dist_source;
    let dest = &paths.store_dist_dest;
    if dest.exists() {
        fs::remove_dir_all(dest)
            .with_context(|| format!("failed to remove old store dist at {}", dest.display()))?;
    }
    copy_dir_recursive(src, dest)?;
    println!(
        "Copied desktop-store dist to {}",
        dest.display()
    );
    Ok(())
}

fn print_frontend_help() {
    println!(
        "ato-desktop xtask frontend\n\n\
         Subcommands:\n  \
           build [-- <vite-args...>]  Install dependencies with pnpm and build frontend/dist\n  \
           dev   [-- <vite-args...>]  Install dependencies with pnpm and start the Vite dev server\n\n\
         Examples:\n  \
           cargo run --manifest-path xtask/Cargo.toml -- frontend build\n  \
           cargo run --manifest-path xtask/Cargo.toml -- frontend dev -- --host 127.0.0.1 --port 4174\n"
    );
}

fn frontend_build(forwarded_args: &[String]) -> Result<()> {
    let paths = WorkspacePaths::discover()?;
    install_frontend_dependencies(&paths)?;
    run_frontend_pnpm(&paths, &["run", "build"], forwarded_args)
}

fn frontend_dev(forwarded_args: &[String]) -> Result<()> {
    let paths = WorkspacePaths::discover()?;
    install_frontend_dependencies(&paths)?;
    run_frontend_pnpm(&paths, &["run", "dev"], forwarded_args)
}

fn install_frontend_dependencies(paths: &WorkspacePaths) -> Result<()> {
    run_frontend_pnpm(paths, &["install", "--frozen-lockfile"], &[])
}

fn run_frontend_pnpm(
    paths: &WorkspacePaths,
    args: &[&str],
    forwarded_args: &[String],
) -> Result<()> {
    let mut command = Command::new("pnpm");
    command.current_dir(&paths.frontend_root);
    for arg in args {
        command.arg(arg);
    }
    for arg in forwarded_args {
        command.arg(arg);
    }
    let status = command
        .status()
        .with_context(|| format!("failed to invoke pnpm in {}", paths.frontend_root.display()))?;
    if !status.success() {
        bail!(
            "pnpm command failed in {} with status {}",
            paths.frontend_root.display(),
            status
        );
    }
    Ok(())
}

fn normalize_passthrough_args(mut args: Vec<String>) -> Vec<String> {
    if matches!(args.first().map(String::as_str), Some("--")) {
        args.remove(0);
    }
    args
}

/// Build the `ato-desktop` and `ato` binaries for a given Rust target.
/// Returns the *target staging root*, populated as either:
///   - macOS:   `dist/<target>/Ato Desktop.app/Contents/...`
///   - Windows: `dist/<target>/Ato/{ato-desktop.exe, bin/ato.exe, assets/}`
///   - Linux:   `dist/<target>/AppDir/usr/{bin/{ato-desktop,ato},share/applications/...}`
fn bundle_windows_app(target: &str) -> Result<PathBuf> {
    let rust_target = match target {
        "windows-x86_64" => "x86_64-pc-windows-msvc",
        other => bail!("unsupported windows target: {}", other),
    };
    let paths = WorkspacePaths::discover()?;
    run_cargo_build(
        &paths.desktop_manifest,
        "ato-desktop",
        rust_target,
        &paths.target_root,
    )?;
    run_cargo_build(&paths.ato_manifest, "ato", rust_target, &paths.target_root)?;
    run_cargo_build(
        &paths.nacelle_manifest,
        "nacelle",
        rust_target,
        &paths.target_root,
    )?;

    let staging = paths.desktop_root.join("dist").join(target).join("Ato");
    if staging.exists() {
        fs::remove_dir_all(&staging)
            .with_context(|| format!("failed to remove old staging {}", staging.display()))?;
    }
    let bin_dir = staging.join("bin");
    let assets_dir = staging.join("assets");
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(&assets_dir)?;

    let profile_dir = format!("{rust_target}/release");
    let desktop_exe = paths.target_root.join(&profile_dir).join("ato-desktop.exe");
    let helper_exe = paths.target_root.join(&profile_dir).join("ato.exe");
    let nacelle_exe = paths.target_root.join(&profile_dir).join("nacelle.exe");
    fs::copy(&desktop_exe, staging.join("ato-desktop.exe")).with_context(|| {
        format!(
            "failed to copy {} to staging — was the cross-build successful?",
            desktop_exe.display()
        )
    })?;
    fs::copy(&helper_exe, bin_dir.join("ato.exe"))
        .with_context(|| format!("failed to copy {} to staging", helper_exe.display()))?;
    fs::copy(&nacelle_exe, bin_dir.join("nacelle.exe"))
        .with_context(|| format!("failed to copy {} to staging", nacelle_exe.display()))?;
    copy_dir_recursive(&paths.desktop_root.join("assets"), &assets_dir)?;

    println!("Staged Windows install tree at {}", staging.display());
    Ok(staging)
}

fn bundle_linux_app(target: &str) -> Result<PathBuf> {
    let rust_target = match target {
        "linux-x86_64" => "x86_64-unknown-linux-gnu",
        "linux-arm64" => "aarch64-unknown-linux-gnu",
        other => bail!("unsupported linux target: {}", other),
    };
    let paths = WorkspacePaths::discover()?;
    run_cargo_build(
        &paths.desktop_manifest,
        "ato-desktop",
        rust_target,
        &paths.target_root,
    )?;
    run_cargo_build(&paths.ato_manifest, "ato", rust_target, &paths.target_root)?;
    run_cargo_build(
        &paths.nacelle_manifest,
        "nacelle",
        rust_target,
        &paths.target_root,
    )?;

    let staging = paths.desktop_root.join("dist").join(target).join("AppDir");
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    let bin_dir = staging.join("usr").join("bin");
    let app_dir = staging.join("usr").join("share").join("applications");
    let metainfo_dir = staging.join("usr").join("share").join("metainfo");
    let assets_dir = staging
        .join("usr")
        .join("share")
        .join("ato-desktop")
        .join("assets");
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(&app_dir)?;
    fs::create_dir_all(&metainfo_dir)?;
    fs::create_dir_all(&assets_dir)?;

    let profile_dir = format!("{rust_target}/release");
    fs::copy(
        paths.target_root.join(&profile_dir).join("ato-desktop"),
        bin_dir.join("ato-desktop"),
    )
    .context("failed to stage ato-desktop binary")?;
    fs::copy(
        paths.target_root.join(&profile_dir).join("ato"),
        bin_dir.join("ato"),
    )
    .context("failed to stage ato helper binary")?;
    fs::copy(
        paths.target_root.join(&profile_dir).join("nacelle"),
        bin_dir.join("nacelle"),
    )
    .context("failed to stage nacelle binary")?;

    // Copy declarative installer metadata if present. These ship from
    // PR-8's installer/ folder and let `xdg-mime` pick up our URL
    // schemes after install.
    let installer_dir = paths.desktop_root.join("installer");
    let desktop_file = installer_dir.join("ato-desktop.desktop");
    if desktop_file.exists() {
        fs::copy(&desktop_file, app_dir.join("ato-desktop.desktop"))?;
    }
    let appdata_file = installer_dir.join("ato-desktop.appdata.xml");
    if appdata_file.exists() {
        fs::copy(&appdata_file, metainfo_dir.join("ato-desktop.appdata.xml"))?;
    }
    // appimagetool requires the icon referenced by `Icon=` in the .desktop
    // file to live at the AppDir root. Stage the placeholder PNG from
    // installer/ for v0.1.0.
    let icon_file = installer_dir.join("ato-desktop.png");
    if icon_file.exists() {
        fs::copy(&icon_file, staging.join("ato-desktop.png"))?;
        let icon_share_dir = staging
            .join("usr")
            .join("share")
            .join("icons")
            .join("hicolor")
            .join("256x256")
            .join("apps");
        fs::create_dir_all(&icon_share_dir)?;
        fs::copy(&icon_file, icon_share_dir.join("ato-desktop.png"))?;
    }
    copy_dir_recursive(&paths.desktop_root.join("assets"), &assets_dir)?;

    println!("Staged Linux AppDir at {}", staging.display());
    Ok(staging)
}

/// Wrap a Windows staging tree in an .msi via WiX. v0.5 ships
/// unsigned per docs/v0.5-distribution-plan.md D-4 / L10 — the
/// signtool path is scaffolded below but gated on
/// `WINDOWS_CODESIGN_PFX` so it stays a no-op until v0.5.x lands the
/// EV cert.
fn package_msi(staging: &Path, target: &str) -> Result<()> {
    let wxs = locate_wix_source()?;
    let arch = match target {
        "windows-x86_64" => "x64",
        other => bail!("unsupported msi target: {}", other),
    };
    let version = env!("CARGO_PKG_VERSION");
    let dist_dir = staging.parent().context("staging path has no parent")?;
    let obj_path = dist_dir.join("ato.wixobj");
    let msi_path = dist_dir.join(format!("Ato-Desktop-{version}-{target}.msi"));

    // candle = compile .wxs → .wixobj
    let status = Command::new("candle")
        .args(["-arch", arch])
        .arg(format!(
            "-dStagingDir={}",
            staging.to_str().context("staging path is not UTF-8")?
        ))
        .arg(format!("-dProductVersion={version}"))
        .arg("-out")
        .arg(&obj_path)
        .arg(&wxs)
        .status()
        .context("failed to invoke `candle` — install WiX Toolset 3.x and ensure it is on PATH")?;
    if !status.success() {
        bail!("candle failed for {} ({})", wxs.display(), status);
    }

    let status = Command::new("light")
        .args(["-ext", "WixUIExtension", "-ext", "WixUtilExtension"])
        .arg("-out")
        .arg(&msi_path)
        .arg(&obj_path)
        .status()
        .context("failed to invoke `light` — install WiX Toolset 3.x")?;
    if !status.success() {
        bail!("light failed ({status})");
    }

    // Optional signtool — only runs when both env vars are set. v0.5
    // intentionally leaves these unset (D-4) so CI builds an unsigned
    // MSI; v0.5.x will populate WINDOWS_CODESIGN_PFX after EV cert
    // procurement.
    if let (Some(pfx), Some(pwd)) = (
        std::env::var("WINDOWS_CODESIGN_PFX")
            .ok()
            .filter(|s| !s.is_empty()),
        std::env::var("WINDOWS_CODESIGN_PASSWORD")
            .ok()
            .filter(|s| !s.is_empty()),
    ) {
        let status = Command::new("signtool")
            .args([
                "sign",
                "/fd",
                "SHA256",
                "/td",
                "SHA256",
                "/tr",
                "http://timestamp.digicert.com",
                "/f",
                &pfx,
                "/p",
                &pwd,
            ])
            .arg(&msi_path)
            .status()
            .context("failed to invoke signtool")?;
        if !status.success() {
            bail!("signtool failed ({status})");
        }
        println!("Signed MSI with {pfx}");
    } else {
        println!(
            "package_msi: signtool skipped (WINDOWS_CODESIGN_PFX / \
             WINDOWS_CODESIGN_PASSWORD not set) — v0.5 default per \
             docs/v0.5-distribution-plan.md L10"
        );
    }

    println!("Built {}", msi_path.display());
    Ok(())
}

fn locate_wix_source() -> Result<PathBuf> {
    let xtask_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = xtask_root
        .parent()
        .context("xtask must live under apps/ato-desktop/xtask")?
        .join("installer")
        .join("wix.wxs");
    if !path.exists() {
        bail!(
            "WiX source missing at {} — expected from PR-7 scaffolding",
            path.display()
        );
    }
    Ok(path)
}

/// Wrap a Linux AppDir staging tree into a single AppImage. Uses
/// `appimagetool` from PATH (CI installs it via apt or
/// AppImageKit-continuous releases). The staging tree must already
/// contain `usr/bin/ato-desktop`, `usr/share/applications/
/// ato-desktop.desktop`, and an `AppRun` entry — the latter is
/// generated here as a thin shell wrapper to avoid hand-editing.
fn package_appimage(staging: &Path, target: &str) -> Result<()> {
    let arch = match target {
        "linux-x86_64" => "x86_64",
        "linux-arm64" => "aarch64",
        other => bail!("unsupported appimage target: {}", other),
    };

    // AppRun is the AppImage entry point; it must live at the AppDir
    // root and exec the real binary. Keep this wrapper tiny so it is
    // obvious what AppImage does at runtime.
    let app_run = staging.join("AppRun");
    fs::write(
        &app_run,
        "#!/bin/sh\n\
         HERE=\"$(dirname \"$(readlink -f \"$0\")\")\"\n\
         exec \"$HERE/usr/bin/ato-desktop\" \"$@\"\n",
    )
    .context("failed to write AppRun")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&app_run)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&app_run, perms)?;
    }

    // appimagetool requires a `.desktop` file at the AppDir root that
    // matches the one under usr/share/applications. Copy it up.
    let inner_desktop = staging
        .join("usr")
        .join("share")
        .join("applications")
        .join("ato-desktop.desktop");
    if inner_desktop.exists() {
        fs::copy(&inner_desktop, staging.join("ato-desktop.desktop")).ok();
    }

    let version = env!("CARGO_PKG_VERSION");
    let out_path = staging
        .parent()
        .context("staging has no parent")?
        .join(format!("Ato-Desktop-{version}-{arch}.AppImage"));

    let status = Command::new("appimagetool")
        .arg(staging)
        .arg(&out_path)
        .env("ARCH", arch)
        .status()
        .context(
            "failed to invoke appimagetool — install from \
             https://github.com/AppImage/AppImageKit/releases and ensure it is on PATH",
        )?;
    if !status.success() {
        bail!("appimagetool failed ({status})");
    }

    println!("Built {}", out_path.display());
    Ok(())
}

/// Code-signing strategy resolved from environment.
///
/// The two modes share the same hardened-runtime entitlements file
/// (installer/entitlements.plist) on purpose: switching to Developer
/// ID later is a single env-var flip, not a runtime-profile change.
enum CodesignMode {
    /// Ad-hoc — `codesign --force --sign -` with hardened-runtime.
    /// This is the v0.5 default per docs/v0.5-distribution-plan.md D-3.
    AdHoc,
    /// Developer ID Application identity. Triggered when
    /// MAC_DEVELOPER_ID_NAME env is set (e.g.
    /// "Developer ID Application: Acme, Inc. (ABCDE12345)").
    DeveloperId(String),
}

fn resolved_codesign_mode() -> CodesignMode {
    match std::env::var("MAC_DEVELOPER_ID_NAME") {
        Ok(name) if !name.trim().is_empty() => CodesignMode::DeveloperId(name),
        _ => CodesignMode::AdHoc,
    }
}

/// Sign the bundle using the inside-out order required by Apple's
/// hardened-runtime model: helper binaries first, then the outer
/// `.app`. A flat sweep would produce a verifier error because the
/// outer bundle's seal must include the (already-signed) inner
/// helpers.
fn codesign_bundle(bundle: &Path) -> Result<()> {
    let mode = resolved_codesign_mode();
    let entitlements = locate_entitlements()?;
    let helper = bundle.join("Contents").join("Helpers").join("ato");
    let nacelle = bundle.join("Contents").join("Helpers").join("nacelle");
    let main_binary = bundle.join("Contents").join("MacOS").join("ato-desktop");

    if !helper.exists() {
        bail!(
            "expected helper binary at {} — did `bundle` complete successfully?",
            helper.display()
        );
    }
    if !nacelle.exists() {
        bail!(
            "expected nacelle binary at {} — did `bundle` complete successfully?",
            nacelle.display()
        );
    }
    if !main_binary.exists() {
        bail!("expected main binary at {}", main_binary.display());
    }

    // Inside-out: Helpers/{ato,nacelle} → MacOS/ato-desktop → outer .app
    codesign_path(&helper, &mode, &entitlements)?;
    codesign_path(&nacelle, &mode, &entitlements)?;
    codesign_path(&main_binary, &mode, &entitlements)?;
    codesign_path(bundle, &mode, &entitlements)?;
    println!(
        "Signed {} with {}",
        bundle.display(),
        match &mode {
            CodesignMode::AdHoc => "ad-hoc identity (-)".to_string(),
            CodesignMode::DeveloperId(name) => format!("Developer ID '{name}'"),
        }
    );
    Ok(())
}

fn codesign_path(path: &Path, mode: &CodesignMode, entitlements: &Path) -> Result<()> {
    let identity = match mode {
        CodesignMode::AdHoc => "-",
        CodesignMode::DeveloperId(name) => name.as_str(),
    };
    let path_str = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("path is not valid UTF-8"))?;
    let entitlements_str = entitlements
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("entitlements path is not valid UTF-8"))?;
    let status = Command::new("codesign")
        .args([
            "--force",
            "--timestamp=none", // notarize step re-signs with timestamp
            "--options=runtime",
            "--entitlements",
            entitlements_str,
            "--sign",
            identity,
            path_str,
        ])
        .status()
        .with_context(|| format!("failed to invoke codesign for {}", path.display()))?;

    if !status.success() {
        bail!("codesign failed for {} ({})", path.display(), status);
    }
    Ok(())
}

fn locate_entitlements() -> Result<PathBuf> {
    let xtask_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = xtask_root
        .parent()
        .map(Path::to_path_buf)
        .context("xtask must live under apps/ato-desktop/xtask")?
        .join("installer")
        .join("entitlements.plist");
    if !path.exists() {
        bail!(
            "entitlements file missing at {} (expected from PR-3 scaffolding)",
            path.display()
        );
    }
    Ok(path)
}

/// Submit the bundle to Apple's notary service. No-op when the three
/// required env vars are not all set — this is the v0.5 default and
/// matches docs/v0.5-distribution-plan.md PR-4 ("no Apple secrets
/// required for v0.5").
fn notarize_bundle(bundle: &Path) -> Result<()> {
    let apple_id = std::env::var("APPLE_ID").ok().filter(|s| !s.is_empty());
    let app_pwd = std::env::var("APPLE_APP_SPECIFIC_PASSWORD")
        .ok()
        .filter(|s| !s.is_empty());
    let team_id = std::env::var("APPLE_TEAM_ID")
        .ok()
        .filter(|s| !s.is_empty());
    let (Some(apple_id), Some(app_pwd), Some(team_id)) = (apple_id, app_pwd, team_id) else {
        println!(
            "notarize: skipped (no Apple credentials — set APPLE_ID, \
             APPLE_APP_SPECIFIC_PASSWORD, APPLE_TEAM_ID to enable)"
        );
        return Ok(());
    };

    if !bundle.exists() {
        bail!("bundle path does not exist: {}", bundle.display());
    }

    // notarytool expects a zipped .app — produce it next to the bundle.
    let zip_path = bundle.with_extension("zip");
    if zip_path.exists() {
        fs::remove_file(&zip_path).ok();
    }
    let bundle_dir = bundle
        .parent()
        .context("cannot determine parent of bundle path")?;
    let bundle_name = bundle
        .file_name()
        .and_then(|s| s.to_str())
        .context("bundle path has no file name")?;
    let zip_str = zip_path.to_str().context("zip path is not valid UTF-8")?;

    let status = Command::new("ditto")
        .args(["-c", "-k", "--keepParent", bundle_name, zip_str])
        .current_dir(bundle_dir)
        .status()
        .context("failed to invoke ditto to zip the bundle")?;
    if !status.success() {
        bail!("ditto failed with status {}", status);
    }

    let status = Command::new("xcrun")
        .args([
            "notarytool",
            "submit",
            zip_str,
            "--apple-id",
            &apple_id,
            "--password",
            &app_pwd,
            "--team-id",
            &team_id,
            "--wait",
        ])
        .status()
        .context("failed to invoke xcrun notarytool")?;
    if !status.success() {
        bail!("notarytool submit failed with status {}", status);
    }

    let status = Command::new("xcrun")
        .args([
            "stapler",
            "staple",
            bundle.to_str().context("bundle path is not valid UTF-8")?,
        ])
        .status()
        .context("failed to invoke xcrun stapler")?;
    if !status.success() {
        bail!("stapler staple failed with status {}", status);
    }

    println!("notarize: stapled ticket onto {}", bundle.display());
    Ok(())
}

/// Wrap the .app in a curl-friendly `.zip` using `ditto -c -k --keepParent`.
///
/// `ditto` is the Apple-recommended way to archive a code-signed bundle
/// because it preserves extended attributes (notably the codesign xattrs
/// `com.apple.cs.CodeDirectory` etc.) and HFS+ metadata. `tar -cz` strips
/// some of those on extraction; `zip(1)` does too. We also avoid `.dmg`
/// here because Safari taints downloaded `.dmg` files with the
/// `com.apple.quarantine` attribute, which forces every user through
/// the Gatekeeper warning. `curl` of a `.zip` followed by `unzip` does
/// not get tagged.
fn package_macos_zip(bundle: &Path, target: &str) -> Result<()> {
    if !bundle.exists() {
        bail!("bundle does not exist: {}", bundle.display());
    }
    let arch = match target {
        "darwin-arm64" => "arm64",
        "darwin-x86_64" => "x86_64",
        other => bail!("unsupported macOS zip target: {}", other),
    };
    let version = env!("CARGO_PKG_VERSION");
    let zip_path = bundle
        .parent()
        .context("cannot determine parent of bundle path")?
        .join(format!("Ato-Desktop-{version}-darwin-{arch}.zip"));

    if zip_path.exists() {
        fs::remove_file(&zip_path).ok();
    }

    let status = Command::new("ditto")
        .args([
            "-c",
            "-k",
            "--keepParent",
            bundle.to_str().context("bundle path is not UTF-8")?,
            zip_path.to_str().context("zip path is not UTF-8")?,
        ])
        .status()
        .context("failed to invoke ditto")?;
    if !status.success() {
        bail!("ditto failed with status {}", status);
    }

    println!("Built {}", zip_path.display());
    Ok(())
}

/// Wrap the Windows staging tree (`Ato/`) in a curl-friendly `.zip`.
///
/// install.sh on Windows can `Expand-Archive` the result; the `.msi`
/// remains available for users who prefer system-wide MSI install.
fn package_windows_zip(staging: &Path, target: &str) -> Result<()> {
    if !staging.exists() {
        bail!("staging dir does not exist: {}", staging.display());
    }
    if target != "windows-x86_64" {
        bail!("unsupported windows zip target: {}", target);
    }
    let version = env!("CARGO_PKG_VERSION");
    let zip_path = staging
        .parent()
        .context("cannot determine parent of staging path")?
        .join(format!("Ato-Desktop-{version}-windows-x86_64.zip"));

    if zip_path.exists() {
        fs::remove_file(&zip_path).ok();
    }

    // Use `tar -a -c -f out.zip <dir>` — the modern bsdtar that ships
    // with Windows 10+ recognises `.zip` from the extension and emits
    // a real zip archive. ditto is macOS-only so we cannot reuse it
    // here. Tar runs the cwd at staging's parent so the archive's
    // top-level entry is `Ato/`, matching the .app drag-drop UX.
    let parent = staging.parent().context("staging has no parent")?;
    let leaf = staging
        .file_name()
        .context("staging path has no file name")?;
    let status = Command::new("tar")
        .arg("-a")
        .arg("-c")
        .arg("-f")
        .arg(&zip_path)
        .arg("-C")
        .arg(parent)
        .arg(leaf)
        .status()
        .context("failed to invoke tar (expected bsdtar with -a flag on windows-2022)")?;
    if !status.success() {
        bail!("tar zip failed with status {}", status);
    }

    println!("Built {}", zip_path.display());
    Ok(())
}

fn bundle_macos_app(target: &str) -> Result<PathBuf> {
    let spec = MacTarget::parse(target)?;
    let paths = WorkspacePaths::discover()?;

    run_cargo_build(
        &paths.desktop_manifest,
        "ato-desktop",
        &spec.rust_target,
        &paths.target_root,
    )?;
    run_cargo_build(
        &paths.ato_manifest,
        "ato",
        &spec.rust_target,
        &paths.target_root,
    )?;
    run_cargo_build(
        &paths.nacelle_manifest,
        "nacelle",
        &spec.rust_target,
        &paths.target_root,
    )?;

    let bundle_root = paths
        .desktop_root
        .join("dist")
        .join(target)
        .join(format!("{}.app", APP_NAME));
    if bundle_root.exists() {
        fs::remove_dir_all(&bundle_root)
            .with_context(|| format!("failed to remove old bundle {}", bundle_root.display()))?;
    }

    let contents_dir = bundle_root.join("Contents");
    let macos_dir = contents_dir.join("MacOS");
    let resources_dir = contents_dir.join("Resources");
    let helpers_dir = contents_dir.join("Helpers");

    fs::create_dir_all(&macos_dir)
        .with_context(|| format!("failed to create {}", macos_dir.display()))?;
    fs::create_dir_all(&resources_dir)
        .with_context(|| format!("failed to create {}", resources_dir.display()))?;
    fs::create_dir_all(&helpers_dir)
        .with_context(|| format!("failed to create {}", helpers_dir.display()))?;

    let profile_dir = PathBuf::from(&spec.profile_dir);

    let desktop_binary = paths.target_root.join(&profile_dir).join("ato-desktop");
    let helper_binary = paths.target_root.join(&profile_dir).join("ato");
    let nacelle_binary = paths.target_root.join(&profile_dir).join("nacelle");

    let app_binary_path = macos_dir.join("ato-desktop");
    let helper_path = helpers_dir.join("ato");
    let nacelle_path = helpers_dir.join("nacelle");
    copy_executable(&desktop_binary, &app_binary_path)?;
    strip_macos_binary(&app_binary_path)?;
    copy_executable(&helper_binary, &helper_path)?;
    strip_macos_binary(&helper_path)?;
    copy_executable(&nacelle_binary, &nacelle_path)?;
    strip_macos_binary(&nacelle_path)?;

    copy_dir_recursive(
        &paths.desktop_root.join("assets"),
        &resources_dir.join("assets"),
    )?;

    // Place AppIcon.icns at Contents/Resources/ root (referenced from
    // CFBundleIconFile in Info.plist). The same .icns also lives under
    // Resources/assets/ via the copy above; keeping both is harmless and
    // avoids a special-case skip in copy_dir_recursive.
    let icns_src = paths.desktop_root.join("assets").join("AppIcon.icns");
    if icns_src.exists() {
        fs::copy(&icns_src, resources_dir.join("AppIcon.icns"))
            .context("failed to copy AppIcon.icns to Contents/Resources")?;
    }

    let plist = render_info_plist(&spec.bundle_version);
    fs::write(contents_dir.join("Info.plist"), plist).context("failed to write Info.plist")?;

    println!("Bundled {}", bundle_root.display());
    println!("  app binary: {}", app_binary_path.display());
    println!("  helper: {}", helper_path.display());
    println!("  nacelle: {}", nacelle_path.display());
    println!("  assets: {}", resources_dir.join("assets").display());

    Ok(bundle_root)
}

fn run_cargo_build(
    manifest_path: &Path,
    bin: &str,
    rust_target: &str,
    target_dir: &Path,
) -> Result<()> {
    let manifest_path_str = manifest_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("manifest path is not valid UTF-8"))?;
    let target_dir_str = target_dir
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("target_dir is not valid UTF-8"))?;
    // ato-desktop is excluded from the workspace (root Cargo.toml `exclude`)
    // so without an explicit --target-dir its build artifacts land in
    // `crates/ato-desktop/target/...` while ato + nacelle land in the
    // workspace `target/...`. Forcing a single target_dir for all three
    // builds is the simplest way to keep `paths.target_root` (used by the
    // staging copies below) honest.
    let status = Command::new("cargo")
        .args([
            "build",
            "--release",
            "--manifest-path",
            manifest_path_str,
            "--bin",
            bin,
            "--target",
            rust_target,
            "--target-dir",
            target_dir_str,
        ])
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .with_context(|| format!("failed to run cargo build for {}", manifest_path.display()))?;

    if !status.success() {
        bail!(
            "cargo build failed for {} (bin {}) with status {}",
            manifest_path.display(),
            bin,
            status
        );
    }

    Ok(())
}

fn copy_executable(from: &Path, to: &Path) -> Result<()> {
    fs::copy(from, to)
        .with_context(|| format!("failed to copy {} to {}", from.display(), to.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(to)
            .with_context(|| format!("failed to read metadata for {}", to.display()))?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(to, permissions)
            .with_context(|| format!("failed to chmod {}", to.display()))?;
    }
    Ok(())
}

fn strip_macos_binary(path: &Path) -> Result<()> {
    let status = Command::new("strip")
        .args([
            "-x",
            path.to_str()
                .ok_or_else(|| anyhow::anyhow!("binary path is not valid UTF-8"))?,
        ])
        .status()
        .with_context(|| format!("failed to run strip for {}", path.display()))?;

    if !status.success() {
        bail!("strip failed for {} with status {}", path.display(), status);
    }

    Ok(())
}

fn copy_dir_recursive(from: &Path, to: &Path) -> Result<()> {
    if !from.is_dir() {
        bail!("directory does not exist: {}", from.display());
    }

    fs::create_dir_all(to).with_context(|| format!("failed to create {}", to.display()))?;
    for entry in fs::read_dir(from).with_context(|| format!("failed to read {}", from.display()))? {
        let entry = entry?;
        let path = entry.path();
        let destination = to.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &destination)?;
        } else {
            fs::copy(&path, &destination).with_context(|| {
                format!(
                    "failed to copy {} to {}",
                    path.display(),
                    destination.display()
                )
            })?;
        }
    }

    Ok(())
}

fn render_info_plist(version: &str) -> String {
    format!(
        r#"<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<plist version=\"1.0\">
  <dict>
    <key>CFBundleName</key>
    <string>{APP_NAME}</string>
    <key>CFBundleDisplayName</key>
    <string>{APP_NAME}</string>
    <key>CFBundleIdentifier</key>
    <string>{APP_IDENTIFIER}</string>
    <key>CFBundleExecutable</key>
    <string>ato-desktop</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleVersion</key>
    <string>{version}</string>
    <key>CFBundleShortVersionString</key>
    <string>{version}</string>
    <key>CFBundleURLTypes</key>
    <array>
      <dict>
        <key>CFBundleTypeRole</key>
        <string>Editor</string>
        <key>CFBundleURLName</key>
        <string>run.ato.desktop.callback</string>
        <key>CFBundleURLSchemes</key>
        <array>
          <string>ato</string>
        </array>
      </dict>
      <dict>
        <key>CFBundleTypeRole</key>
        <string>Viewer</string>
        <key>CFBundleURLName</key>
        <string>run.ato.desktop.capsule</string>
        <key>CFBundleURLSchemes</key>
        <array>
          <string>capsule</string>
        </array>
      </dict>
    </array>
    <key>LSMinimumSystemVersion</key>
    <string>13.0</string>
  </dict>
</plist>
"#
    )
}

struct WorkspacePaths {
    desktop_root: PathBuf,
    desktop_manifest: PathBuf,
    frontend_root: PathBuf,
    ato_manifest: PathBuf,
    nacelle_manifest: PathBuf,
    target_root: PathBuf,
    store_root: PathBuf,
    store_dist_source: PathBuf,
    store_dist_dest: PathBuf,
}

impl WorkspacePaths {
    fn discover() -> Result<Self> {
        let xtask_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let desktop_root = xtask_root
            .parent()
            .map(Path::to_path_buf)
            .context("xtask crate must live under <repo>/crates/ato-desktop/xtask")?;
        let repo_root = desktop_root
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .context("failed to resolve repository root from crates/ato-desktop")?;
        // Layouts probed in priority order:
        //   1. monorepo:           <repo>/crates/ato-cli (current canonical)
        //   2. legacy split-repo:  <repo>/apps/ato-cli (pre-M1)
        //   3. CI sibling clone:   <repo>/../ato-cli (legacy release workflow)
        // The fallback chain lets a single xtask binary build correctly
        // in both the monorepo and any leftover mirror checkout while M7
        // archives the old repos.
        let ato_root = {
            let monorepo = repo_root.join("crates").join("ato-cli");
            if monorepo.exists() {
                monorepo
            } else {
                let legacy_apps = repo_root.join("apps").join("ato-cli");
                if legacy_apps.exists() {
                    legacy_apps
                } else {
                    desktop_root
                        .parent()
                        .map(|p| p.join("ato-cli"))
                        .unwrap_or_else(|| repo_root.join("ato-cli"))
                }
            }
        };
        let desktop_manifest = desktop_root.join("Cargo.toml");
        let frontend_root = desktop_root.join("frontend");
        let ato_manifest = ato_root.join("Cargo.toml");
        // nacelle lives at <repo>/crates/nacelle in the monorepo.
        let nacelle_manifest = repo_root.join("crates").join("nacelle").join("Cargo.toml");
        let target_root = repo_root.join("target");
        // ato-web lives as a sibling of the ato repo root
        // (apps/ato-web alongside apps/ato).
        let store_root = repo_root
            .parent()
            .map(|p| p.join("ato-web"))
            .unwrap_or_else(|| repo_root.join("..").join("ato-web"));
        let store_dist_source = store_root.join("dist-desktop");
        let store_dist_dest = desktop_root
            .join("assets")
            .join("system")
            .join("ato-store")
            .join("dist");

        Ok(Self {
            desktop_root,
            desktop_manifest,
            frontend_root,
            ato_manifest,
            nacelle_manifest,
            target_root,
            store_root,
            store_dist_source,
            store_dist_dest,
        })
    }
}

struct MacTarget {
    rust_target: String,
    profile_dir: String,
    bundle_version: String,
}

impl MacTarget {
    fn parse(input: &str) -> Result<Self> {
        let rust_target = match input {
            "darwin-arm64" => "aarch64-apple-darwin",
            "darwin-x86_64" => "x86_64-apple-darwin",
            other => bail!("unsupported bundle target: {}", other),
        }
        .to_string();

        Ok(Self {
            profile_dir: format!("{}{}", rust_target, "/release"),
            rust_target,
            bundle_version: env!("CARGO_PKG_VERSION").to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{normalize_passthrough_args, render_info_plist, MacTarget};

    #[test]
    fn parses_supported_targets() {
        let parsed = MacTarget::parse("darwin-arm64").expect("target should parse");
        assert_eq!(parsed.rust_target, "aarch64-apple-darwin");
        assert_eq!(parsed.profile_dir, "aarch64-apple-darwin/release");
    }

    #[test]
    fn info_plist_contains_identifier() {
        let plist = render_info_plist("1.2.3");
        assert!(plist.contains("run.ato.desktop"));
        assert!(plist.contains("1.2.3"));
    }

    #[test]
    fn normalize_passthrough_args_strips_delimiter() {
        assert_eq!(
            normalize_passthrough_args(vec![
                "--".to_string(),
                "--host".to_string(),
                "127.0.0.1".to_string(),
            ]),
            vec!["--host".to_string(), "127.0.0.1".to_string()]
        );
    }
}
