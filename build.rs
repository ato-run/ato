use std::env;
use std::io::ErrorKind;
use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;
use std::process::{Command, ExitStatus};

#[cfg(windows)]
const DEFAULT_UI_NODE_VERSION: &str = "20.12.0";

enum NpmProgram {
    System,
    #[cfg(windows)]
    Bootstrapped(PathBuf),
}

fn npm_status(
    npm_program: &NpmProgram,
    ui_dir: &Path,
    args: &[&str],
) -> std::io::Result<ExitStatus> {
    let mut command = match npm_program {
        NpmProgram::System => Command::new("npm"),
        #[cfg(windows)]
        NpmProgram::Bootstrapped(path) => Command::new(path),
    };

    command.arg("--prefix").arg(ui_dir).args(args).status()
}

fn command_available(program: &str) -> bool {
    match Command::new(program)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(_) => true,
        Err(err) if err.kind() == ErrorKind::NotFound => false,
        Err(_) => false,
    }
}

#[cfg(windows)]
fn ui_node_version() -> String {
    env::var("ATO_UI_NODE_VERSION")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_UI_NODE_VERSION.to_string())
}

fn resolve_npm_program(toolchain_dir: &Path) -> std::io::Result<NpmProgram> {
    if command_available("npm") {
        return Ok(NpmProgram::System);
    }

    #[cfg(windows)]
    {
        let version = ui_node_version();
        let npm_path = ensure_windows_bootstrapped_npm(toolchain_dir, &version)?;
        return Ok(NpmProgram::Bootstrapped(npm_path));
    }

    #[cfg(not(windows))]
    {
        let _ = toolchain_dir;
        Err(std::io::Error::new(
            ErrorKind::NotFound,
            "npm was not found on PATH",
        ))
    }
}

fn ui_dist_available(ui_dir: &Path) -> bool {
    ui_dir.join("dist").join("index.html").is_file()
}

fn warn_skip_ui_build_without_npm(ui_dir: &Path, err: &std::io::Error) {
    println!(
        "cargo:warning=Skipping UI build because npm is unavailable ({err}) and prebuilt assets already exist under {}",
        ui_dir.join("dist").display()
    );
}

#[cfg(windows)]
fn ensure_windows_bootstrapped_npm(
    toolchain_dir: &Path,
    version: &str,
) -> std::io::Result<PathBuf> {
    let arch = match env::var("CARGO_CFG_TARGET_ARCH").ok().as_deref() {
        Some("x86_64") => "x64",
        Some("aarch64") => "arm64",
        Some(other) => {
            return Err(std::io::Error::new(
                ErrorKind::Other,
                format!("unsupported Windows architecture for UI npm bootstrap: {other}"),
            ))
        }
        None => "x64",
    };

    let base_name = format!("node-v{version}-win-{arch}");
    let archive_path = toolchain_dir.join(format!("{base_name}.zip"));
    let sums_path = toolchain_dir.join(format!("{base_name}.SHASUMS256.txt"));
    let extract_root = toolchain_dir.join(&base_name);
    let npm_cmd = extract_root.join("npm.cmd");

    if npm_cmd.is_file() {
        println!(
            "cargo:warning=Using bootstrapped npm from {}",
            npm_cmd.display()
        );
        return Ok(npm_cmd);
    }

    std::fs::create_dir_all(toolchain_dir)?;

    let archive_url = format!("https://nodejs.org/dist/v{version}/{base_name}.zip");
    let sums_url = format!("https://nodejs.org/dist/v{version}/SHASUMS256.txt");
    println!(
        "cargo:warning=Bootstrapping portable Node.js/npm {} for UI build because system npm is missing",
        version
    );

    run_powershell(&format!(
        "$ProgressPreference='SilentlyContinue'; Invoke-WebRequest -Uri '{}' -OutFile '{}'",
        powershell_quote(&archive_url),
        powershell_quote_path(&archive_path),
    ))?;
    run_powershell(&format!(
        "$ProgressPreference='SilentlyContinue'; Invoke-WebRequest -Uri '{}' -OutFile '{}'",
        powershell_quote(&sums_url),
        powershell_quote_path(&sums_path),
    ))?;
    run_powershell(&format!(
        "$expected=(Get-Content '{}' | Where-Object {{ $_ -match ' {base_name}\\.zip$' }} | ForEach-Object {{ ($_ -split '\\s+')[0] }} | Select-Object -First 1); if (-not $expected) {{ throw 'missing checksum for {base_name}.zip' }}; $sha256=[System.Security.Cryptography.SHA256]::Create(); try {{ $bytes=[System.IO.File]::ReadAllBytes('{}'); $actual=[System.BitConverter]::ToString($sha256.ComputeHash($bytes)).Replace('-', '').ToLower() }} finally {{ $sha256.Dispose() }}; if ($actual -ne $expected.ToLower()) {{ throw \"checksum mismatch: expected=$expected actual=$actual\" }}",
        powershell_quote_path(&sums_path),
        powershell_quote_path(&archive_path),
    ))?;
    run_powershell(&format!(
        "if (Test-Path '{}') {{ Remove-Item -Recurse -Force '{}' }}; Expand-Archive -Path '{}' -DestinationPath '{}' -Force",
        powershell_quote_path(&extract_root),
        powershell_quote_path(&extract_root),
        powershell_quote_path(&archive_path),
        powershell_quote_path(toolchain_dir),
    ))?;

    if !npm_cmd.is_file() {
        return Err(std::io::Error::new(
            ErrorKind::NotFound,
            format!(
                "bootstrapped npm was not found after extraction: {}",
                npm_cmd.display()
            ),
        ));
    }

    Ok(npm_cmd)
}

#[cfg(windows)]
fn run_powershell(script: &str) -> std::io::Result<()> {
    let status = Command::new("powershell")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::new(
            ErrorKind::Other,
            format!("PowerShell command failed with status {status}"),
        ))
    }
}

#[cfg(windows)]
fn powershell_quote(value: &str) -> String {
    value.replace('\'', "''")
}

#[cfg(windows)]
fn powershell_quote_path(path: &Path) -> String {
    powershell_quote(&path.display().to_string())
}

fn main() {
    let ui_dir = Path::new("apps/ato-store-local");
    let ui_toolchain_dir = Path::new("target").join("ui-toolchain");
    let ui_src = ui_dir.join("src");
    let ui_public = ui_dir.join("public");
    let ui_package = ui_dir.join("package.json");
    let ui_lockfile = ui_dir.join("package-lock.json");
    let ui_vite_bin = ui_dir
        .join("node_modules")
        .join(".bin")
        .join(if cfg!(windows) { "vite.cmd" } else { "vite" });

    println!("cargo:rerun-if-env-changed=ATO_SKIP_UI_BUILD");
    println!("cargo:rerun-if-env-changed=ATO_UI_NODE_VERSION");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", ui_package.display());
    if ui_lockfile.exists() {
        println!("cargo:rerun-if-changed={}", ui_lockfile.display());
    }
    if ui_src.exists() {
        println!("cargo:rerun-if-changed={}", ui_src.display());
    }
    if ui_public.exists() {
        println!("cargo:rerun-if-changed={}", ui_public.display());
    }

    if env::var("ATO_SKIP_UI_BUILD")
        .ok()
        .as_deref()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    {
        println!("cargo:warning=Skipping UI build because ATO_SKIP_UI_BUILD is set");
        return;
    }

    if !ui_package.exists() {
        println!(
            "cargo:warning=Skipping UI build because {} was not found",
            ui_package.display()
        );
        return;
    }

    let npm_program = match resolve_npm_program(&ui_toolchain_dir) {
        Ok(program) => Some(program),
        Err(err) if err.kind() == ErrorKind::NotFound && ui_dist_available(ui_dir) => {
            warn_skip_ui_build_without_npm(ui_dir, &err);
            return;
        }
        Err(err) => panic!(
            "Failed to resolve npm for UI build: {}. Install Node.js/npm or set ATO_SKIP_UI_BUILD=1.",
            err
        ),
    };
    let npm_program = npm_program.expect("npm program resolved");

    if !ui_vite_bin.exists() {
        let install_args: &[&str] = if ui_lockfile.exists() {
            &["ci", "--include=dev"]
        } else {
            &["install", "--include=dev"]
        };
        println!(
            "cargo:warning=Installing UI dependencies (including Vite) because {} is missing",
            ui_vite_bin.display()
        );
        match npm_status(&npm_program, ui_dir, install_args) {
            Ok(status) if status.success() => {}
            Ok(status) => panic!(
                "UI dependency install failed (status: {}). Run `npm install --prefix apps/ato-store-local` and retry.",
                status
            ),
            Err(err) => panic!(
                "Failed to execute npm for UI dependency install: {}. Install Node.js/npm or set ATO_SKIP_UI_BUILD=1.",
                err
            ),
        }

        if !ui_vite_bin.exists() {
            panic!(
                "UI dependency install completed but {} is still missing. Ensure npm devDependencies are enabled and retry.",
                ui_vite_bin.display()
            );
        }
    }

    let status = npm_status(&npm_program, ui_dir, &["run", "build"]);

    match status {
        Ok(status) if status.success() => {}
        Ok(status) => panic!(
            "UI build failed (status: {}). Run `npm install --prefix apps/ato-store-local` and retry.",
            status
        ),
        Err(err) => panic!(
            "Failed to execute npm for UI build: {}. Install Node.js/npm or set ATO_SKIP_UI_BUILD=1.",
            err
        ),
    }
}
