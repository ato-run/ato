use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

const APP_NAME: &str = "Ato Desktop";
const APP_IDENTIFIER: &str = "run.ato.desktop";
const DEFAULT_TARGET: &str = "darwin-arm64";
const BUNDLED_SYSTEM_ASSET_EXCLUDED_DIRS: &[&str] =
    &["node_modules", ".vite", ".astro", ".next", "target"];

#[derive(Debug, Clone, PartialEq, Eq)]
enum BundleTarget {
    DarwinArm64,
    DarwinX86_64,
    WindowsX86_64,
    LinuxX86_64,
    LinuxArm64,
}

impl BundleTarget {
    const DEFAULT: Self = Self::DarwinArm64;

    const ALL: &'static [Self] = &[Self::DarwinArm64, Self::DarwinX86_64, Self::WindowsX86_64, Self::LinuxX86_64, Self::LinuxArm64];

    fn as_str(&self) -> &'static str {
        match self {
            Self::DarwinArm64 => "darwin-arm64",
            Self::DarwinX86_64 => "darwin-x86_64",
            Self::WindowsX86_64 => "windows-x86_64",
            Self::LinuxX86_64 => "linux-x86_64",
            Self::LinuxArm64 => "linux-arm64",
        }
    }

    fn parse(raw: &str) -> Result<Self> {
        match raw {
            "darwin-arm64" => Ok(Self::DarwinArm64),
            "darwin-x86_64" => Ok(Self::DarwinX86_64),
            "windows-x86_64" => Ok(Self::WindowsX86_64),
            "linux-x86_64" => Ok(Self::LinuxX86_64),
            "linux-arm64" => Ok(Self::LinuxArm64),
            other => bail!("unsupported target: {}", other),
        }
    }

    fn help_list() -> String {
        Self::ALL.iter().map(|t| t.as_str()).collect::<Vec<_>>().join(", ")
    }
}

fn main() -> Result<()> {
    let all: Vec<String> = std::env::args().skip(1).collect();

    let Some(cmd) = all.first().map(String::as_str) else {
        print_help();
        return Ok(());
    };

    match cmd {
        "bundle" => {
            let forwarded = &all[1..];

            let mut target = BundleTarget::DEFAULT;
            let mut sign = false;
            let mut do_notarize = false;
            let mut do_zip = false;
            let mut do_msi = false;
            let mut do_appimage = false;
            let mut helper_source_arg: Option<String> = None;
            let mut helper_artifact_dir_arg: Option<String> = None;
            let mut i = 0;
            while i < forwarded.len() {
                let arg = &forwarded[i];
                i += 1;
                match arg.as_str() {
                    "--target" => {
                        let raw = forwarded
                            .get(i)
                            .context("--target requires a value such as darwin-arm64")?
                            .clone();
                        target = BundleTarget::parse(&raw)?;
                        i += 1;
                    }
                    "--sign" => sign = true,
                    "--notarize" => do_notarize = true,
                    "--zip" => do_zip = true,
                    "--msi" => do_msi = true,
                    "--appimage" => do_appimage = true,
                    "--helper-source" => {
                        helper_source_arg = Some(
                            forwarded
                                .get(i)
                                .context("--helper-source requires a value: local or release")?
                                .clone(),
                        );
                        i += 1;
                    }
                    "--helper-artifact-dir" => {
                        helper_artifact_dir_arg = Some(
                            forwarded
                                .get(i)
                                .context("--helper-artifact-dir requires a directory path")?
                                .clone(),
                        );
                        i += 1;
                    }
                    other => bail!("unsupported xtask argument: {}", other),
                }
            }
            // Resolve where the bundled `ato` / `nacelle` helpers come from.
            // `local` (default) builds them from source; `release` consumes
            // prebuilt cargo-dist artifacts so Desktop packaging never
            // rebuilds the CLI/sidecar (issue #366). Made explicit on purpose
            // — a release build must never silently fall back to rebuilding.
            let helper_source =
                resolve_helper_source(helper_source_arg, helper_artifact_dir_arg)?;
            // Dispatch by target family. Each platform has its own
            // staging layout — keeping them in distinct functions
            // makes the per-platform invariants (Helpers/ato vs
            // bin\ato.exe vs usr/bin/ato) easy to verify.
            //
            // macOS .zip via `ditto -c -k --keepParent` preserves the
            // codesign xattrs that hdiutil/.dmg lose; .dmg is also
            // quarantine-tainted when downloaded via Safari, so the
            // zip path is now the canonical install.sh delivery.
            match target {
                BundleTarget::DarwinArm64 | BundleTarget::DarwinX86_64 => {
                    let bundle = bundle_macos_app(target.as_str(), &helper_source)?;
                    if sign {
                        codesign_bundle(&bundle)?;
                    }
                    if do_notarize {
                        notarize_bundle(&bundle)?;
                    }
                    if do_zip {
                        package_macos_zip(&bundle, target.as_str())?;
                    }
                    Ok(())
                }
                BundleTarget::WindowsX86_64 => {
                    let staging = bundle_windows_app(target.as_str(), &helper_source)?;
                    if do_msi {
                        package_msi(&staging, target.as_str())?;
                    }
                    if do_zip {
                        package_windows_zip(&staging, target.as_str())?;
                    }
                    Ok(())
                }
                BundleTarget::LinuxX86_64 | BundleTarget::LinuxArm64 => {
                    let staging = bundle_linux_app(target.as_str(), &helper_source)?;
                    if do_appimage {
                        package_appimage(&staging, target.as_str())?;
                    }
                    Ok(())
                }
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
           bundle [--target TARGET] [--sign] [--notarize] [--zip] [--msi] [--appimage]\n         \
                  [--helper-source local|release] [--helper-artifact-dir DIR]\n  \
           notarize <bundle>     Submit an .app to Apple notary (no-op without APPLE_* env)\n  \
           zip      <path>       Wrap a .app bundle (macOS) or staging dir (Windows) in a .zip\n  \
           msi      <staging>    Wrap a Windows staging tree in an .msi via WiX (candle/light)\n  \
           appimage <staging>    Wrap a Linux staging tree in an .AppImage via appimagetool\n\n\
         Targets:\n  \
            {targets} (default: {default})\n\n\
         Helper source (bundled private `ato` + `nacelle`):\n  \
            - local   (default) build ato + nacelle from the workspace\n  \
            - release           consume prebuilt cargo-dist artifacts from\n                     \
                      --helper-artifact-dir / ATO_HELPER_ARTIFACT_DIR (no rebuild).\n            \
                Env equivalents: ATO_DESKTOP_HELPER_SOURCE, ATO_HELPER_ARTIFACT_DIR.\n  \
            Bundled `ato`/`nacelle` are PRIVATE Desktop-internal helpers — bundling\n  \
            does NOT expose `ato` on the user's shell PATH (a separate CLI-expose step\n  \
            does that), and `nacelle` is never placed on PATH. ato-netd is always built\n  \
            locally (it is not a released cargo-dist artifact).\n\n\
         macOS code-signing modes (resolved at runtime):\n  \
            - if MAC_DEVELOPER_ID_NAME is set: real Developer ID (hardened runtime + entitlements)\n  \
            - else:                            ad-hoc (`codesign --sign -`) — v0.5 default\n\n\
         Windows: signtool integration is scaffolded but env-gated; v0.5 ships unsigned (L10).\n",
        targets = BundleTarget::help_list(),
        default = BundleTarget::DEFAULT.as_str(),
    );
}

/// Build the `ato-desktop` and helper binaries for a given Rust target.
/// Returns the *target staging root*, populated as either:
///   - macOS:   `dist/<target>/Ato Desktop.app/Contents/...`
///   - Windows: `dist/<target>/Ato/{ato-desktop.exe, bin/ato.exe, assets/}`
///   - Linux:   `dist/<target>/AppDir/usr/{bin/{ato-desktop,ato,ato-netd},share/applications/...}`
fn bundle_windows_app(target: &str, helper_source: &HelperSource) -> Result<PathBuf> {
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
    // ato + nacelle come from `helper_source` (built locally or consumed
    // from released cargo-dist artifacts). ato-desktop is always built here.
    let helpers = stage_helper_binaries(helper_source, target, rust_target, &paths)?;

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
    fs::copy(&desktop_exe, staging.join("ato-desktop.exe")).with_context(|| {
        format!(
            "failed to copy {} to staging — was the cross-build successful?",
            desktop_exe.display()
        )
    })?;
    fs::copy(&helpers.ato, bin_dir.join("ato.exe"))
        .with_context(|| format!("failed to copy {} to staging", helpers.ato.display()))?;
    fs::copy(&helpers.nacelle, bin_dir.join("nacelle.exe"))
        .with_context(|| format!("failed to copy {} to staging", helpers.nacelle.display()))?;
    copy_bundled_assets(&paths.desktop_root.join("assets"), &assets_dir)?;
    assert_windows_staging_layout(&staging)?;

    println!("Staged Windows install tree at {}", staging.display());
    Ok(staging)
}

fn assert_windows_staging_layout(staging: &Path) -> Result<()> {
    let required_files = [
        staging.join("ato-desktop.exe"),
        staging.join("bin").join("ato.exe"),
        staging.join("bin").join("nacelle.exe"),
    ];
    for path in required_files {
        if !path.is_file() {
            bail!(
                "Windows staging is missing required file {}",
                path.display()
            );
        }
    }

    let assets = staging.join("assets");
    if !assets.is_dir() {
        bail!(
            "Windows staging is missing required assets directory {}",
            assets.display()
        );
    }
    let mut entries = fs::read_dir(&assets)
        .with_context(|| format!("failed to read assets directory {}", assets.display()))?;
    if entries.next().is_none() {
        bail!(
            "Windows staging assets directory is empty: {}",
            assets.display()
        );
    }
    Ok(())
}

fn bundle_linux_app(target: &str, helper_source: &HelperSource) -> Result<PathBuf> {
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
    // ato + nacelle come from `helper_source`. ato-desktop and ato-netd are
    // always built locally — ato-netd is not a released cargo-dist artifact
    // (dist-workspace.toml ships only ato-cli + nacelle), so it has no
    // release source to consume (issue #366).
    let helpers = stage_helper_binaries(helper_source, target, rust_target, &paths)?;
    run_cargo_build(
        &paths.netd_manifest,
        "ato-netd",
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
    copy_executable(
        &paths.target_root.join(&profile_dir).join("ato-desktop"),
        &bin_dir.join("ato-desktop"),
    )
    .context("failed to stage ato-desktop binary")?;
    copy_executable(&helpers.ato, &bin_dir.join("ato")).context("failed to stage ato helper binary")?;
    copy_executable(&helpers.nacelle, &bin_dir.join("nacelle"))
        .context("failed to stage nacelle binary")?;
    copy_executable(
        &paths.target_root.join(&profile_dir).join("ato-netd"),
        &bin_dir.join("ato-netd"),
    )
    .context("failed to stage ato-netd binary")?;

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
    copy_bundled_assets(&paths.desktop_root.join("assets"), &assets_dir)?;
    assert_required_paths(
        &staging,
        &[
            "usr/bin/ato",
            "usr/bin/nacelle",
            "usr/bin/ato-netd",
            "usr/share/ato-desktop/assets",
        ],
    )?;

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
    let assets_wxs_path = dist_dir.join("ato-assets.wxs");
    let assets_obj_path = dist_dir.join("ato-assets.wixobj");
    let msi_path = dist_dir.join(format!("Ato-Desktop-{version}-{target}.msi"));
    assert_windows_staging_layout(staging)?;

    let assets_dir = staging.join("assets");
    let status = Command::new("heat")
        .arg("dir")
        .arg(&assets_dir)
        .args([
            "-cg",
            "AssetsFiles",
            "-dr",
            "AssetsFolder",
            "-srd",
            "-sreg",
            "-gg",
            "-var",
            "var.StagingAssetsDir",
        ])
        .arg("-out")
        .arg(&assets_wxs_path)
        .status()
        .context("failed to invoke `heat` — install WiX Toolset 3.x and ensure it is on PATH")?;
    if !status.success() {
        bail!("heat failed for {} ({})", assets_dir.display(), status);
    }

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
    let status = Command::new("candle")
        .args(["-arch", arch])
        .arg(format!(
            "-dStagingAssetsDir={}",
            assets_dir
                .to_str()
                .context("assets staging path is not UTF-8")?
        ))
        .arg("-out")
        .arg(&assets_obj_path)
        .arg(&assets_wxs_path)
        .status()
        .context("failed to invoke `candle` for harvested assets")?;
    if !status.success() {
        bail!(
            "candle failed for harvested assets {} ({})",
            assets_wxs_path.display(),
            status
        );
    }

    let status = Command::new("light")
        .args(["-ext", "WixUIExtension", "-ext", "WixUtilExtension"])
        .arg("-out")
        .arg(&msi_path)
        .arg(&obj_path)
        .arg(&assets_obj_path)
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
    let netd = bundle.join("Contents").join("Helpers").join("ato-netd");
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
    if !netd.exists() {
        bail!(
            "expected ato-netd binary at {} — did `bundle` complete successfully?",
            netd.display()
        );
    }
    if !main_binary.exists() {
        bail!("expected main binary at {}", main_binary.display());
    }

    // Inside-out: Helpers/{ato,nacelle,ato-netd} → MacOS/ato-desktop → outer .app
    codesign_path(&helper, &mode, &entitlements)?;
    codesign_path(&nacelle, &mode, &entitlements)?;
    codesign_path(&netd, &mode, &entitlements)?;
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

fn bundle_macos_app(target: &str, helper_source: &HelperSource) -> Result<PathBuf> {
    let spec = MacTarget::parse(target)?;
    let paths = WorkspacePaths::discover()?;

    run_cargo_build(
        &paths.desktop_manifest,
        "ato-desktop",
        &spec.rust_target,
        &paths.target_root,
    )?;
    // ato + nacelle come from `helper_source`. ato-desktop and ato-netd are
    // always built locally — ato-netd is not a released cargo-dist artifact
    // (dist-workspace.toml ships only ato-cli + nacelle), so it has no
    // release source to consume (issue #366).
    let helpers = stage_helper_binaries(helper_source, target, &spec.rust_target, &paths)?;
    run_cargo_build(
        &paths.netd_manifest,
        "ato-netd",
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
    let helper_binary = helpers.ato.clone();
    let nacelle_binary = helpers.nacelle.clone();
    let netd_binary = paths.target_root.join(&profile_dir).join("ato-netd");

    let app_binary_path = macos_dir.join("ato-desktop");
    let helper_path = helpers_dir.join("ato");
    let nacelle_path = helpers_dir.join("nacelle");
    let netd_path = helpers_dir.join("ato-netd");
    copy_executable(&desktop_binary, &app_binary_path)?;
    strip_macos_binary(&app_binary_path)?;
    copy_executable(&helper_binary, &helper_path)?;
    strip_macos_binary(&helper_path)?;
    copy_executable(&nacelle_binary, &nacelle_path)?;
    strip_macos_binary(&nacelle_path)?;
    copy_executable(&netd_binary, &netd_path)?;
    strip_macos_binary(&netd_path)?;

    copy_bundled_assets(
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

    assert_required_paths(
        &bundle_root,
        &[
            "Contents/Helpers/ato",
            "Contents/Helpers/nacelle",
            "Contents/Helpers/ato-netd",
            "Contents/Resources/assets",
        ],
    )?;

    let plist = render_info_plist(&spec.bundle_version);
    fs::write(contents_dir.join("Info.plist"), plist).context("failed to write Info.plist")?;

    println!("Bundled {}", bundle_root.display());
    println!("  app binary: {}", app_binary_path.display());
    println!("  helper: {}", helper_path.display());
    println!("  nacelle: {}", nacelle_path.display());
    println!("  netd: {}", netd_path.display());
    println!("  assets: {}", resources_dir.join("assets").display());

    Ok(bundle_root)
}

/// Where the bundled `ato` / `nacelle` helper binaries come from.
///
/// This distinction is deliberately explicit (issue #366): a release build
/// must consume prebuilt artifacts and must never silently fall back to
/// rebuilding the CLI/sidecar from source.
///
/// - `Local`   — build `ato` + `nacelle` from the workspace (dev default).
/// - `Release` — consume the prebuilt cargo-dist archives that the main
///   release pipeline (`release.yml`) already published, from a local
///   directory the caller pre-populated (no network access here — the
///   workflow downloads the artifacts; xtask only stages them).
#[derive(Debug, Clone)]
enum HelperSource {
    Local,
    Release { artifact_dir: PathBuf },
}

/// Resolved source paths for the two consumed helpers. ato-desktop and
/// ato-netd are out of band — they are always built locally.
struct StagedHelpers {
    ato: PathBuf,
    nacelle: PathBuf,
}

const HELPER_SOURCE_ENV: &str = "ATO_DESKTOP_HELPER_SOURCE";
const HELPER_ARTIFACT_DIR_ENV: &str = "ATO_HELPER_ARTIFACT_DIR";

/// Resolve the helper source from CLI flags, falling back to env vars, then
/// to `local`. Precedence: `--helper-source` > `ATO_DESKTOP_HELPER_SOURCE`
/// > default `local`; likewise `--helper-artifact-dir` >
/// `ATO_HELPER_ARTIFACT_DIR`.
fn resolve_helper_source(
    cli_source: Option<String>,
    cli_artifact_dir: Option<String>,
) -> Result<HelperSource> {
    let source = cli_source
        .or_else(|| std::env::var(HELPER_SOURCE_ENV).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "local".to_string());

    match source.as_str() {
        "local" => Ok(HelperSource::Local),
        "release" => {
            let artifact_dir = cli_artifact_dir
                .or_else(|| std::env::var(HELPER_ARTIFACT_DIR_ENV).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .with_context(|| {
                    format!(
                        "--helper-source=release requires the prebuilt artifacts directory: \
                         pass --helper-artifact-dir <dir> or set {HELPER_ARTIFACT_DIR_ENV}"
                    )
                })?;
            if !artifact_dir.is_dir() {
                bail!(
                    "helper artifact directory does not exist: {} \
                     (download the released ato-cli/nacelle archives there first)",
                    artifact_dir.display()
                );
            }
            Ok(HelperSource::Release { artifact_dir })
        }
        other => bail!(
            "unsupported --helper-source '{other}': expected 'local' or 'release'"
        ),
    }
}

/// Produce the `ato` and `nacelle` helper binaries for `target`, either by
/// building them locally or by consuming prebuilt release artifacts.
fn stage_helper_binaries(
    source: &HelperSource,
    target: &str,
    rust_target: &str,
    paths: &WorkspacePaths,
) -> Result<StagedHelpers> {
    match source {
        HelperSource::Local => {
            run_cargo_build(&paths.ato_manifest, "ato", rust_target, &paths.target_root)?;
            run_cargo_build(
                &paths.nacelle_manifest,
                "nacelle",
                rust_target,
                &paths.target_root,
            )?;
            let profile_dir = format!("{rust_target}/release");
            Ok(StagedHelpers {
                ato: paths
                    .target_root
                    .join(&profile_dir)
                    .join(helper_exe_name("ato", target)),
                nacelle: paths
                    .target_root
                    .join(&profile_dir)
                    .join(helper_exe_name("nacelle", target)),
            })
        }
        HelperSource::Release { artifact_dir } => {
            println!(
                "helper-source=release: consuming prebuilt ato/nacelle from {} \
                 (no rebuild)",
                artifact_dir.display()
            );
            let unpack_root = paths.desktop_root.join("dist").join(target).join(".helpers");
            if unpack_root.exists() {
                fs::remove_dir_all(&unpack_root).with_context(|| {
                    format!("failed to clean helper unpack dir {}", unpack_root.display())
                })?;
            }
            fs::create_dir_all(&unpack_root)?;
            let ato = consume_release_helper(
                &["ato-cli", "ato"],
                "ato",
                target,
                artifact_dir,
                &unpack_root,
            )?;
            let nacelle = consume_release_helper(
                &["nacelle"],
                "nacelle",
                target,
                artifact_dir,
                &unpack_root,
            )?;
            Ok(StagedHelpers { ato, nacelle })
        }
    }
}

/// cargo-dist Rust target-triple for a desktop bundle target.
fn cargo_dist_triple(target: &str) -> Result<&'static str> {
    Ok(match target {
        "darwin-arm64" => "aarch64-apple-darwin",
        "darwin-x86_64" => "x86_64-apple-darwin",
        "windows-x86_64" => "x86_64-pc-windows-msvc",
        "linux-x86_64" => "x86_64-unknown-linux-gnu",
        "linux-arm64" => "aarch64-unknown-linux-gnu",
        other => bail!("no cargo-dist triple for target {other}"),
    })
}

/// Archive extension cargo-dist uses for a target: `.zip` on Windows,
/// `.tar.xz` everywhere else.
fn helper_archive_ext(target: &str) -> &'static str {
    if target.starts_with("windows-") {
        "zip"
    } else {
        "tar.xz"
    }
}

/// Platform-correct on-disk executable name for a helper stem: `ato.exe` on
/// Windows targets, `ato` otherwise. Keyed off the *target* (not the build
/// host) so cross-target reasoning stays correct.
fn helper_exe_name(stem: &str, target: &str) -> String {
    if target.starts_with("windows-") {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    }
}

/// Locate, verify, unpack, and normalize a single released helper archive.
///
/// `pkg_candidates` is the set of cargo-dist app names the binary might ship
/// under (e.g. the `ato` binary lives in the `ato-cli` package archive).
/// Returns the path to the extracted, correctly-named executable.
fn consume_release_helper(
    pkg_candidates: &[&str],
    bin_stem: &str,
    target: &str,
    artifact_dir: &Path,
    unpack_root: &Path,
) -> Result<PathBuf> {
    let triple = cargo_dist_triple(target)?;
    let ext = helper_archive_ext(target);
    let archive = find_release_archive(pkg_candidates, triple, ext, artifact_dir)
        .with_context(|| {
            format!(
                "no released {bin_stem} artifact for {target} ({triple}) found in {}. \
                 Expected an archive like {}-{triple}.{ext} (download it from the \
                 GitHub release before bundling with --helper-source=release).",
                artifact_dir.display(),
                pkg_candidates.first().copied().unwrap_or(bin_stem),
            )
        })?;

    verify_release_checksum(&archive)?;

    let dest = unpack_root.join(bin_stem);
    fs::create_dir_all(&dest)?;
    extract_archive(&archive, &dest)?;

    let exe_name = helper_exe_name(bin_stem, target);
    let binary = find_file_named(&dest, &exe_name).with_context(|| {
        format!(
            "extracted {} but found no '{exe_name}' inside it",
            archive.display()
        )
    })?;
    println!(
        "  staged {bin_stem} from {} -> {}",
        archive.file_name().and_then(|s| s.to_str()).unwrap_or("?"),
        binary.display()
    );
    Ok(binary)
}

/// First archive in `artifact_dir` matching `{candidate}-…{triple}.{ext}`.
fn find_release_archive(
    pkg_candidates: &[&str],
    triple: &str,
    ext: &str,
    artifact_dir: &Path,
) -> Result<PathBuf> {
    let suffix = format!("{triple}.{ext}");
    for candidate in pkg_candidates {
        let prefix = format!("{candidate}-");
        for entry in fs::read_dir(artifact_dir)
            .with_context(|| format!("failed to read {}", artifact_dir.display()))?
        {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(&prefix) && name.ends_with(&suffix) {
                return Ok(entry.path());
            }
        }
    }
    bail!("no matching archive")
}

/// Verify `<archive>.sha256` if present. cargo-dist writes one checksum file
/// per artifact; verification is best-effort ("if available" per #366) so a
/// missing checksum warns rather than fails.
fn verify_release_checksum(archive: &Path) -> Result<()> {
    let mut sha_path = archive.as_os_str().to_owned();
    sha_path.push(".sha256");
    let sha_path = PathBuf::from(sha_path);
    if !sha_path.is_file() {
        println!(
            "  WARNING: no checksum file at {} — skipping verification",
            sha_path.display()
        );
        return Ok(());
    }
    let expected = fs::read_to_string(&sha_path)
        .with_context(|| format!("failed to read {}", sha_path.display()))?;
    let expected = expected
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if expected.len() != 64 {
        bail!(
            "checksum file {} did not contain a 64-char sha256 hex digest",
            sha_path.display()
        );
    }
    let actual = sha256_hex(archive)?;
    if actual != expected {
        bail!(
            "checksum mismatch for {}: expected {expected}, got {actual}",
            archive.display()
        );
    }
    println!("  verified sha256 of {}", archive.display());
    Ok(())
}

/// Streaming sha256 of a file, lowercase hex.
fn sha256_hex(path: &Path) -> Result<String> {
    let mut file =
        fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Extract a `.tar.xz` or `.zip` archive into `dest` using the system `tar`.
/// bsdtar (macOS/Windows) and GNU tar (Linux) both auto-detect xz via `-xf`;
/// bsdtar also reads `.zip`, which covers the Windows artifacts.
fn extract_archive(archive: &Path, dest: &Path) -> Result<()> {
    let status = Command::new("tar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .status()
        .with_context(|| format!("failed to invoke tar to extract {}", archive.display()))?;
    if !status.success() {
        bail!("tar failed to extract {} ({status})", archive.display());
    }
    Ok(())
}

/// Depth-first search for a file with exactly `name` under `root`.
fn find_file_named(root: &Path, name: &str) -> Result<PathBuf> {
    for entry in
        fs::read_dir(root).with_context(|| format!("failed to read {}", root.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            if let Ok(found) = find_file_named(&path, name) {
                return Ok(found);
            }
        } else if entry.file_name().to_string_lossy() == name {
            return Ok(path);
        }
    }
    bail!("'{name}' not found under {}", root.display())
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
    // `crates/desktop/target/...` while ato + nacelle land in the
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
        // xtask now authoritatively provisions the bundled helpers (see
        // `stage_helper_binaries`), so ato-desktop's build.rs must NOT also
        // spawn a nested `cargo build -p ato-cli -p nacelle`. Without this,
        // the ato-desktop build and the nested helper build contend for the
        // same `--target-dir` cargo lock and deadlock (both target
        // `<root>/target`). CI already sets this; doing it here makes
        // `cargo xtask bundle <target>` work out of the box locally too.
        .env("ATO_DESKTOP_SKIP_HELPER_BUILD", "1")
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

fn copy_bundled_assets(from: &Path, to: &Path) -> Result<()> {
    if !from.is_dir() {
        bail!("directory does not exist: {}", from.display());
    }

    fs::create_dir_all(to).with_context(|| format!("failed to create {}", to.display()))?;
    for entry in fs::read_dir(from).with_context(|| format!("failed to read {}", from.display()))? {
        let entry = entry?;
        let path = entry.path();
        let destination = to.join(entry.file_name());
        if path.is_dir() {
            if entry.file_name() == "system" {
                copy_dir_recursive_excluding(
                    &path,
                    &destination,
                    BUNDLED_SYSTEM_ASSET_EXCLUDED_DIRS,
                )?;
            } else {
                copy_dir_recursive(&path, &destination)?;
            }
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

fn assert_required_paths(root: &Path, required_paths: &[&str]) -> Result<()> {
    for relative_path in required_paths {
        let path = root.join(relative_path);
        if !path.exists() {
            bail!(
                "expected bundled path at {} — staging is incomplete",
                path.display()
            );
        }
    }

    Ok(())
}

fn copy_dir_recursive(from: &Path, to: &Path) -> Result<()> {
    copy_dir_recursive_excluding(from, to, &[])
}

fn copy_dir_recursive_excluding(from: &Path, to: &Path, excluded_dirs: &[&str]) -> Result<()> {
    if !from.is_dir() {
        bail!("directory does not exist: {}", from.display());
    }

    fs::create_dir_all(to).with_context(|| format!("failed to create {}", to.display()))?;
    for entry in fs::read_dir(from).with_context(|| format!("failed to read {}", from.display()))? {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if path.is_dir() && excluded_dirs.iter().any(|excluded| *excluded == file_name) {
            continue;
        }
        let destination = to.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive_excluding(&path, &destination, excluded_dirs)?;
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
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
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
    ato_manifest: PathBuf,
    nacelle_manifest: PathBuf,
    netd_manifest: PathBuf,
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
            .context("xtask crate must live under <repo>/crates/desktop/xtask")?;
        let repo_root = desktop_root
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .context("failed to resolve repository root from crates/ato-desktop")?;
        // Layouts probed in priority order:
        //   1. monorepo:           <repo>/crates/cli (current canonical)
        //   2. legacy split-repo:  <repo>/apps/ato-cli (pre-M1)
        //   3. CI sibling clone:   <repo>/../ato-cli (legacy release workflow)
        // The fallback chain lets a single xtask binary build correctly
        // in both the monorepo and any leftover mirror checkout while M7
        // archives the old repos.
        let ato_root = {
            let monorepo = repo_root.join("crates").join("cli");
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
        let ato_manifest = ato_root.join("Cargo.toml");
        // nacelle lives at <repo>/crates/nacelle in the monorepo.
        let nacelle_manifest = repo_root.join("crates").join("nacelle").join("Cargo.toml");
        let netd_manifest = repo_root.join("crates").join("netd").join("Cargo.toml");
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
            ato_manifest,
            nacelle_manifest,
            netd_manifest,
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
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use std::process::Command;

    use super::{
        assert_windows_staging_layout, cargo_dist_triple, consume_release_helper,
        copy_bundled_assets, find_file_named, find_release_archive, helper_archive_ext,
        helper_exe_name, render_info_plist, resolve_helper_source, sha256_hex,
        verify_release_checksum, HelperSource, MacTarget, WorkspacePaths, HELPER_ARTIFACT_DIR_ENV,
        HELPER_SOURCE_ENV,
    };

    fn temp_dir(prefix: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("{prefix}-{}-{suffix}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir should be created");
        dir
    }

    #[test]
    fn parses_supported_targets() {
        let parsed = MacTarget::parse("darwin-arm64").expect("target should parse");
        assert_eq!(parsed.rust_target, "aarch64-apple-darwin");
        assert_eq!(parsed.profile_dir, "aarch64-apple-darwin/release");
    }

    #[test]
    fn info_plist_is_valid_xml() {
        let plist = render_info_plist("1.2.3");
        assert!(
            !plist.contains(r#"\""#),
            "Info.plist must not contain raw-string escape artifacts (backslash-quote)"
        );
        assert!(
            plist.contains(r#"<?xml version="1.0" encoding="UTF-8"?>"#),
            "Info.plist must start with a valid XML declaration"
        );
    }

    #[test]
    fn info_plist_contains_icon_and_identifier() {
        let plist = render_info_plist("1.2.3");
        assert!(plist.contains("run.ato.desktop"));
        assert!(plist.contains("1.2.3"));
        assert!(
            plist.contains("<key>CFBundleIconFile</key>"),
            "Info.plist must declare CFBundleIconFile"
        );
        assert!(
            plist.contains("<string>AppIcon</string>"),
            "Info.plist CFBundleIconFile value must be AppIcon"
        );
    }

    #[test]
    fn windows_staging_assertion_requires_helper_and_assets() {
        let root = test_root("windows-staging-ok");
        let staging = root.join("Ato");
        fs::create_dir_all(staging.join("bin")).unwrap();
        fs::create_dir_all(staging.join("assets")).unwrap();
        fs::write(staging.join("ato-desktop.exe"), "").unwrap();
        fs::write(staging.join("bin").join("ato.exe"), "").unwrap();
        fs::write(staging.join("bin").join("nacelle.exe"), "").unwrap();
        fs::write(staging.join("assets").join("AppIcon.ico"), "").unwrap();

        assert_windows_staging_layout(&staging).unwrap();
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn windows_staging_assertion_rejects_missing_helper() {
        let root = test_root("windows-staging-missing-helper");
        let staging = root.join("Ato");
        fs::create_dir_all(staging.join("assets")).unwrap();
        fs::write(staging.join("ato-desktop.exe"), "").unwrap();
        fs::write(staging.join("assets").join("AppIcon.ico"), "").unwrap();

        let error = assert_windows_staging_layout(&staging).unwrap_err();
        assert!(error.to_string().contains("bin"));
        assert!(error.to_string().contains("ato.exe"));
        fs::remove_dir_all(root).ok();
    }

    fn test_root(name: &str) -> PathBuf {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(".tmp")
            .join(format!(
                "{name}-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
        if root.exists() {
            fs::remove_dir_all(&root).ok();
        }
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn info_plist_does_not_escape_quotes_inside_raw_string() {
        let plist = render_info_plist("1.2.3");
        assert!(plist.contains(r#"<?xml version="1.0" encoding="UTF-8"?>"#));
        assert!(!plist.contains("\\\""));
    }

    #[test]
    fn bundled_assets_exclude_build_time_directories_but_keep_runtime_files() {
        let source = temp_dir("ato-desktop-assets-src");
        let dest = temp_dir("ato-desktop-assets-dst");

        fs::create_dir_all(source.join("system/ato-start/dist"))
            .expect("dist dir should be created");
        fs::create_dir_all(source.join("system/ato-start/node_modules/react"))
            .expect("node_modules dir should be created");
        fs::create_dir_all(source.join("system/ato-start/.astro/cache"))
            .expect(".astro dir should be created");
        fs::create_dir_all(source.join("system/node_modules/vite"))
            .expect("workspace node_modules dir should be created");
        fs::create_dir_all(source.join("preload")).expect("preload dir should be created");

        fs::write(
            source.join("system/ato-start/capsule.toml"),
            "run = \"dist\"\n",
        )
        .expect("capsule manifest should be written");
        fs::write(
            source.join("system/ato-start/dist/index.html"),
            "<html></html>",
        )
        .expect("dist index should be written");
        fs::write(
            source.join("system/ato-start/node_modules/react/index.js"),
            "export {};",
        )
        .expect("react placeholder should be written");
        fs::write(
            source.join("system/ato-start/.astro/cache/manifest.json"),
            "{}",
        )
        .expect(".astro placeholder should be written");
        fs::write(
            source.join("system/node_modules/vite/index.js"),
            "export {};",
        )
        .expect("workspace node_modules placeholder should be written");
        fs::write(source.join("preload/host_bridge.js"), "console.log('ok');")
            .expect("preload script should be written");
        fs::write(source.join("AppIcon.icns"), "icon").expect("icon should be written");

        copy_bundled_assets(&source, &dest).expect("asset copy should succeed");

        assert!(dest.join("system/ato-start/capsule.toml").is_file());
        assert!(dest.join("system/ato-start/dist/index.html").is_file());
        assert!(dest.join("preload/host_bridge.js").is_file());
        assert!(dest.join("AppIcon.icns").is_file());
        assert!(!dest.join("system/ato-start/node_modules").exists());
        assert!(!dest.join("system/ato-start/.astro").exists());
        assert!(!dest.join("system/node_modules").exists());

        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(dest);
    }

    #[test]
    fn workspace_paths_include_netd_manifest() {
        let paths = WorkspacePaths::discover().expect("workspace paths should resolve");
        assert!(
            paths
                .netd_manifest
                .ends_with(Path::new("crates/netd/Cargo.toml")),
            "expected netd manifest path, got {}",
            paths.netd_manifest.display()
        );
    }

    /// Every build manifest the desktop bundler resolves must actually exist.
    /// This is the gate the v0.7.0 release lacked: the `ato-cli->cli` and
    /// `ato-netd->netd` crate renames left `WorkspacePaths::discover` pointing at
    /// `crates/ato-cli` / `crates/ato-netd`, which no longer exist — and because the
    /// xtask is excluded from `rust-ci`, that only surfaced at tag time when the
    /// desktop bundles failed. An existence check (not a path-string match) catches
    /// any future rename regardless of the new name. See ato-run/ato#758.
    #[test]
    fn discovered_crate_manifests_all_exist() {
        let paths = WorkspacePaths::discover().expect("workspace paths should resolve");
        for (label, manifest) in [
            ("ato (cli)", &paths.ato_manifest),
            ("ato-netd (netd)", &paths.netd_manifest),
            ("ato-desktop (desktop)", &paths.desktop_manifest),
            ("nacelle", &paths.nacelle_manifest),
        ] {
            assert!(
                manifest.exists(),
                "{label} build manifest does not exist: {} — a crate rename likely left \
                 a stale path in WorkspacePaths::discover()",
                manifest.display()
            );
        }
    }

    // ---- release helper consumption (issue #366) ----

    #[test]
    fn cargo_dist_triple_maps_every_bundle_target() {
        assert_eq!(
            cargo_dist_triple("darwin-arm64").unwrap(),
            "aarch64-apple-darwin"
        );
        assert_eq!(
            cargo_dist_triple("darwin-x86_64").unwrap(),
            "x86_64-apple-darwin"
        );
        assert_eq!(
            cargo_dist_triple("windows-x86_64").unwrap(),
            "x86_64-pc-windows-msvc"
        );
        assert_eq!(
            cargo_dist_triple("linux-x86_64").unwrap(),
            "x86_64-unknown-linux-gnu"
        );
        assert_eq!(
            cargo_dist_triple("linux-arm64").unwrap(),
            "aarch64-unknown-linux-gnu"
        );
        assert!(cargo_dist_triple("plan9-riscv").is_err());
    }

    #[test]
    fn helper_exe_name_appends_exe_only_on_windows_targets() {
        assert_eq!(helper_exe_name("ato", "windows-x86_64"), "ato.exe");
        assert_eq!(helper_exe_name("nacelle", "windows-x86_64"), "nacelle.exe");
        assert_eq!(helper_exe_name("ato", "darwin-arm64"), "ato");
        assert_eq!(helper_exe_name("nacelle", "linux-x86_64"), "nacelle");
    }

    #[test]
    fn helper_archive_ext_is_zip_on_windows_else_tar_xz() {
        assert_eq!(helper_archive_ext("windows-x86_64"), "zip");
        assert_eq!(helper_archive_ext("darwin-arm64"), "tar.xz");
        assert_eq!(helper_archive_ext("linux-x86_64"), "tar.xz");
    }

    #[test]
    fn resolve_helper_source_defaults_and_explicit_local() {
        assert!(matches!(
            resolve_helper_source(Some("local".to_string()), None).unwrap(),
            HelperSource::Local
        ));
    }

    #[test]
    fn resolve_helper_source_release_requires_artifact_dir() {
        // Ensure the env fallback is not set so the missing-dir path is hit.
        unsafe { std::env::remove_var(HELPER_ARTIFACT_DIR_ENV); }
        unsafe { std::env::remove_var(HELPER_SOURCE_ENV); }
        let err = resolve_helper_source(Some("release".to_string()), None)
            .expect_err("release without an artifact dir must error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains(HELPER_ARTIFACT_DIR_ENV) || msg.contains("--helper-artifact-dir"),
            "error should name how to supply the dir, got: {msg}"
        );
    }

    #[test]
    fn resolve_helper_source_release_rejects_missing_dir() {
        let missing = temp_dir("ato-helper-missing").join("nope");
        let err = resolve_helper_source(
            Some("release".to_string()),
            Some(missing.to_string_lossy().to_string()),
        )
        .expect_err("nonexistent artifact dir must error");
        assert!(format!("{err:#}").contains("does not exist"));
    }

    #[test]
    fn resolve_helper_source_rejects_unknown_value() {
        let err = resolve_helper_source(Some("download".to_string()), None)
            .expect_err("unknown helper source must error");
        assert!(format!("{err:#}").contains("local"));
    }

    #[test]
    fn resolve_helper_source_release_accepts_explicit_dir() {
        let dir = temp_dir("ato-helper-dir");
        let source = resolve_helper_source(
            Some("release".to_string()),
            Some(dir.to_string_lossy().to_string()),
        )
        .expect("release with a real dir should resolve");
        match source {
            HelperSource::Release { artifact_dir } => assert_eq!(artifact_dir, dir),
            HelperSource::Local => panic!("expected Release"),
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn sha256_hex_matches_known_vector() {
        let dir = temp_dir("ato-sha");
        let file = dir.join("abc.txt");
        fs::write(&file, b"abc").unwrap();
        assert_eq!(
            sha256_hex(&file).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn verify_checksum_accepts_match_rejects_mismatch_warns_on_missing() {
        let dir = temp_dir("ato-verify");
        let archive = dir.join("ato-cli-aarch64-apple-darwin.tar.xz");
        fs::write(&archive, b"fake archive bytes").unwrap();
        let digest = sha256_hex(&archive).unwrap();

        // Missing checksum: best-effort, returns Ok.
        verify_release_checksum(&archive).expect("missing checksum should not fail");

        // Matching checksum (cargo-dist format: `<hex>  <filename>`).
        let sha_path = dir.join("ato-cli-aarch64-apple-darwin.tar.xz.sha256");
        fs::write(
            &sha_path,
            format!("{digest}  ato-cli-aarch64-apple-darwin.tar.xz\n"),
        )
        .unwrap();
        verify_release_checksum(&archive).expect("matching checksum should pass");

        // Mismatch.
        fs::write(&sha_path, format!("{}  x\n", "0".repeat(64))).unwrap();
        assert!(verify_release_checksum(&archive).is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn find_release_archive_matches_package_and_triple() {
        let dir = temp_dir("ato-find-archive");
        fs::write(dir.join("nacelle-aarch64-apple-darwin.tar.xz"), b"n").unwrap();
        fs::write(dir.join("ato-cli-aarch64-apple-darwin.tar.xz"), b"a").unwrap();
        fs::write(dir.join("ato-cli-x86_64-pc-windows-msvc.zip"), b"w").unwrap();

        let ato = find_release_archive(
            &["ato-cli", "ato"],
            "aarch64-apple-darwin",
            "tar.xz",
            &dir,
        )
        .expect("should find the ato-cli archive");
        assert_eq!(
            ato.file_name().unwrap().to_string_lossy(),
            "ato-cli-aarch64-apple-darwin.tar.xz"
        );

        let win = find_release_archive(
            &["ato-cli", "ato"],
            "x86_64-pc-windows-msvc",
            "zip",
            &dir,
        )
        .expect("should find the windows zip");
        assert_eq!(
            win.file_name().unwrap().to_string_lossy(),
            "ato-cli-x86_64-pc-windows-msvc.zip"
        );

        assert!(
            find_release_archive(&["nacelle"], "x86_64-unknown-linux-gnu", "tar.xz", &dir)
                .is_err(),
            "no linux nacelle archive present"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn consume_release_helper_errors_actionably_when_artifact_missing() {
        let artifact_dir = temp_dir("ato-empty-artifacts");
        let unpack = temp_dir("ato-unpack");
        let err = consume_release_helper(
            &["ato-cli", "ato"],
            "ato",
            "darwin-arm64",
            &artifact_dir,
            &unpack,
        )
        .expect_err("missing artifact must error");
        let msg = format!("{err:#}");
        assert!(msg.contains("aarch64-apple-darwin"), "msg: {msg}");
        assert!(msg.contains("--helper-source=release"), "msg: {msg}");
        let _ = fs::remove_dir_all(artifact_dir);
        let _ = fs::remove_dir_all(unpack);
    }

    #[test]
    fn find_file_named_searches_recursively() {
        let dir = temp_dir("ato-find-file");
        fs::create_dir_all(dir.join("a/b/c")).unwrap();
        fs::write(dir.join("a/b/c/nacelle"), b"bin").unwrap();
        let found = find_file_named(&dir, "nacelle").expect("should find nested file");
        assert!(found.ends_with("a/b/c/nacelle"));
        assert!(find_file_named(&dir, "missing").is_err());
        let _ = fs::remove_dir_all(dir);
    }

    /// End-to-end consumption: a cargo-dist-style `.tar.xz` containing the
    /// `ato` binary is verified, unpacked, and normalized — with no cargo
    /// build invoked. Proves the release path stages from artifacts alone.
    #[test]
    fn consume_release_helper_round_trips_from_targz_artifact() {
        let work = temp_dir("ato-roundtrip");
        let artifact_dir = work.join("artifacts");
        let payload = work.join("payload");
        fs::create_dir_all(&artifact_dir).unwrap();
        // cargo-dist nests binaries under a per-archive subdir; mirror that
        // so the recursive `find_file_named` is exercised.
        fs::create_dir_all(payload.join("ato-cli-aarch64-apple-darwin")).unwrap();
        fs::write(
            payload.join("ato-cli-aarch64-apple-darwin/ato"),
            b"#!/bin/sh\necho ato\n",
        )
        .unwrap();

        let archive = artifact_dir.join("ato-cli-aarch64-apple-darwin.tar.xz");
        let created = Command::new("tar")
            .arg("-cJf")
            .arg(&archive)
            .arg("-C")
            .arg(&payload)
            .arg("ato-cli-aarch64-apple-darwin")
            .status();
        // bsdtar/GNU tar with xz is expected on dev + CI hosts; if it is
        // genuinely unavailable the rest of the assertion cannot run.
        let created = created.expect("tar should be invokable");
        assert!(created.success(), "tar -cJf failed to build the fixture archive");

        let digest = sha256_hex(&archive).unwrap();
        fs::write(
            artifact_dir.join("ato-cli-aarch64-apple-darwin.tar.xz.sha256"),
            format!("{digest}  ato-cli-aarch64-apple-darwin.tar.xz\n"),
        )
        .unwrap();

        let unpack = work.join("unpack");
        fs::create_dir_all(&unpack).unwrap();
        let staged = consume_release_helper(
            &["ato-cli", "ato"],
            "ato",
            "darwin-arm64",
            &artifact_dir,
            &unpack,
        )
        .expect("release helper should be consumed");
        assert_eq!(staged.file_name().unwrap().to_string_lossy(), "ato");
        assert_eq!(
            fs::read(&staged).unwrap(),
            b"#!/bin/sh\necho ato\n",
            "extracted binary contents must match the artifact"
        );
        let _ = fs::remove_dir_all(work);
    }
}
