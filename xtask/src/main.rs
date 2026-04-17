use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

const APP_NAME: &str = "Ato Desktop";
const APP_IDENTIFIER: &str = "run.ato.desktop";
const DEFAULT_TARGET: &str = "darwin-arm64";

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("bundle") => {
            let mut target = DEFAULT_TARGET.to_string();
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--target" => {
                        target = args
                            .next()
                            .context("--target requires a value such as darwin-arm64")?;
                    }
                    other => bail!("unsupported xtask argument: {}", other),
                }
            }
            bundle_macos_app(&target)
        }
        Some("help") | Some("--help") | Some("-h") | None => {
            print_help();
            Ok(())
        }
        Some(other) => bail!("unsupported xtask command: {}", other),
    }
}

fn print_help() {
    println!(
        "ato-desktop xtask\n\nCommands:\n  bundle [--target darwin-arm64|darwin-x86_64]  Build ato-desktop and emit a macOS .app bundle"
    );
}

fn bundle_macos_app(target: &str) -> Result<()> {
    let spec = MacTarget::parse(target)?;
    let paths = WorkspacePaths::discover()?;

    run_cargo_build(&paths.desktop_manifest, "ato-desktop", &spec.rust_target)?;
    run_cargo_build(&paths.ato_manifest, "ato", &spec.rust_target)?;

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

    let desktop_binary = paths
        .desktop_root
        .join("target")
        .join(&profile_dir)
        .join("ato-desktop");
    let helper_binary = paths.ato_root.join("target").join(&profile_dir).join("ato");

    let app_binary_path = macos_dir.join("ato-desktop");
    let helper_path = helpers_dir.join("ato");
    copy_executable(&desktop_binary, &app_binary_path)?;
    strip_macos_binary(&app_binary_path)?;
    copy_executable(&helper_binary, &helper_path)?;
    strip_macos_binary(&helper_path)?;

    copy_dir_recursive(
        &paths.desktop_root.join("assets"),
        &resources_dir.join("assets"),
    )?;

    let plist = render_info_plist(&spec.bundle_version);
    fs::write(contents_dir.join("Info.plist"), plist).context("failed to write Info.plist")?;

    println!("Bundled {}", bundle_root.display());
    println!("  app binary: {}", app_binary_path.display());
    println!("  helper: {}", helper_path.display());
    println!("  assets: {}", resources_dir.join("assets").display());

    Ok(())
}

fn run_cargo_build(manifest_path: &Path, bin: &str, rust_target: &str) -> Result<()> {
    let manifest_path_str = manifest_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("manifest path is not valid UTF-8"))?;
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
    ato_root: PathBuf,
    ato_manifest: PathBuf,
}

impl WorkspacePaths {
    fn discover() -> Result<Self> {
        let xtask_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let desktop_root = xtask_root
            .parent()
            .map(Path::to_path_buf)
            .context("xtask crate must live under apps/ato-desktop/xtask")?;
        let repo_root = desktop_root
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .context("failed to resolve repository root from apps/ato-desktop")?;
        let ato_root = repo_root.join("apps").join("ato-cli");
        let desktop_manifest = desktop_root.join("Cargo.toml");
        let ato_manifest = ato_root.join("Cargo.toml");

        Ok(Self {
            desktop_root,
            desktop_manifest,
            ato_root,
            ato_manifest,
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
    use super::{render_info_plist, MacTarget};

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
}
