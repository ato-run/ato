use std::collections::BTreeMap;

use super::*;
use capsule_core::bootstrap::{BootstrapAuthorityKind, BootstrapClosureRole};
use tempfile::tempdir;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn assert_json_object_has_keys(value: &serde_json::Value, keys: &[&str]) {
    let object = value.as_object().expect("expected JSON object");
    for key in keys {
        assert!(
            object.contains_key(*key),
            "expected key '{}' in JSON object: {object:?}",
            key
        );
    }
}

fn sample_delivery_toml() -> &'static str {
    r#"schema_version = "0.1"
[artifact]
framework = "tauri"
stage = "unsigned"
target = "darwin/arm64"
input = "MyApp.app"
[finalize]
tool = "codesign"
args = ["--deep", "--force", "--sign", "-", "MyApp.app"]
"#
}

fn sample_fetch_dir(root: &Path) -> Result<PathBuf> {
    sample_fetch_dir_with_mode(root, 0o755)
}

fn sample_nested_delivery_toml() -> &'static str {
    r#"schema_version = "0.1"
[artifact]
framework = "tauri"
stage = "unsigned"
target = "darwin/arm64"
input = "src-tauri/target/release/bundle/macos/My App.app"
[finalize]
tool = "codesign"
args = ["--deep", "--force", "--sign", "-", "src-tauri/target/release/bundle/macos/My App.app"]
"#
}

fn sample_file_delivery_toml() -> String {
    format!(
            "schema_version = \"0.1\"\n[artifact]\nframework = \"tauri\"\nstage = \"unsigned\"\ntarget = \"{}\"\ninput = \"dist/MyApp.exe\"\n[finalize]\ntool = \"signtool\"\nargs = [\"sign\", \"/fd\", \"SHA256\", \"dist/MyApp.exe\"]\n",
            default_delivery_target_for_input("dist/MyApp.exe")
        )
}

fn sample_windows_pe_bytes(is_dll: bool) -> Vec<u8> {
    const SAMPLE_PE_SIZE: usize = 0x400;
    const PE_OFFSET: usize = 0x80;
    const PE32_PLUS_OPTIONAL_HEADER_SIZE: usize = 0xF0;
    const SECTION_ALIGNMENT: u32 = 0x1000;
    const FILE_ALIGNMENT: u32 = 0x200;
    const IMAGE_BASE: u64 = 0x1_4000_0000;
    // IMAGE_FILE_EXECUTABLE_IMAGE | IMAGE_FILE_LARGE_ADDRESS_AWARE
    const EXECUTABLE_CHARACTERISTICS: u16 = 0x0022;
    // EXECUTABLE_CHARACTERISTICS | IMAGE_FILE_DLL
    const DLL_CHARACTERISTICS: u16 = 0x2022;

    let mut bytes = vec![0u8; SAMPLE_PE_SIZE];
    bytes[0..2].copy_from_slice(b"MZ");
    bytes[0x3c..0x40].copy_from_slice(&(PE_OFFSET as u32).to_le_bytes());

    bytes[PE_OFFSET..PE_OFFSET + 4].copy_from_slice(b"PE\0\0");

    let coff_offset = PE_OFFSET + 4;
    bytes[coff_offset..coff_offset + 2].copy_from_slice(&(0x8664u16).to_le_bytes());
    bytes[coff_offset + 2..coff_offset + 4].copy_from_slice(&(1u16).to_le_bytes());
    bytes[coff_offset + 16..coff_offset + 18]
        .copy_from_slice(&(PE32_PLUS_OPTIONAL_HEADER_SIZE as u16).to_le_bytes());
    bytes[coff_offset + 18..coff_offset + 20].copy_from_slice(
        &(if is_dll {
            DLL_CHARACTERISTICS
        } else {
            EXECUTABLE_CHARACTERISTICS
        })
        .to_le_bytes(),
    );

    let optional_offset = coff_offset + 20;
    bytes[optional_offset..optional_offset + 2].copy_from_slice(&(0x20bu16).to_le_bytes());
    bytes[optional_offset + 4..optional_offset + 8].copy_from_slice(&FILE_ALIGNMENT.to_le_bytes());
    bytes[optional_offset + 16..optional_offset + 20]
        .copy_from_slice(&SECTION_ALIGNMENT.to_le_bytes());
    bytes[optional_offset + 20..optional_offset + 24]
        .copy_from_slice(&SECTION_ALIGNMENT.to_le_bytes());
    bytes[optional_offset + 24..optional_offset + 32].copy_from_slice(&IMAGE_BASE.to_le_bytes());
    bytes[optional_offset + 32..optional_offset + 36]
        .copy_from_slice(&SECTION_ALIGNMENT.to_le_bytes());
    bytes[optional_offset + 36..optional_offset + 40]
        .copy_from_slice(&FILE_ALIGNMENT.to_le_bytes());
    bytes[optional_offset + 40..optional_offset + 42].copy_from_slice(&(6u16).to_le_bytes());
    bytes[optional_offset + 48..optional_offset + 50].copy_from_slice(&(6u16).to_le_bytes());
    bytes[optional_offset + 56..optional_offset + 60]
        .copy_from_slice(&(SECTION_ALIGNMENT * 2).to_le_bytes());
    bytes[optional_offset + 60..optional_offset + 64]
        .copy_from_slice(&FILE_ALIGNMENT.to_le_bytes());
    bytes[optional_offset + 68..optional_offset + 70].copy_from_slice(&(3u16).to_le_bytes());
    bytes[optional_offset + 72..optional_offset + 80]
        .copy_from_slice(&(0x10_0000u64).to_le_bytes());
    bytes[optional_offset + 80..optional_offset + 88].copy_from_slice(&(0x1000u64).to_le_bytes());
    bytes[optional_offset + 88..optional_offset + 96]
        .copy_from_slice(&(0x10_0000u64).to_le_bytes());
    bytes[optional_offset + 96..optional_offset + 104].copy_from_slice(&(0x1000u64).to_le_bytes());
    bytes[optional_offset + 108..optional_offset + 112].copy_from_slice(&(16u32).to_le_bytes());

    let section_offset = optional_offset + PE32_PLUS_OPTIONAL_HEADER_SIZE;
    bytes[section_offset..section_offset + 8].copy_from_slice(b".text\0\0\0");
    bytes[section_offset + 8..section_offset + 12].copy_from_slice(&(1u32).to_le_bytes());
    bytes[section_offset + 12..section_offset + 16]
        .copy_from_slice(&SECTION_ALIGNMENT.to_le_bytes());
    bytes[section_offset + 16..section_offset + 20].copy_from_slice(&FILE_ALIGNMENT.to_le_bytes());
    bytes[section_offset + 20..section_offset + 24].copy_from_slice(&FILE_ALIGNMENT.to_le_bytes());
    bytes[section_offset + 36..section_offset + 40]
        .copy_from_slice(&(0x6000_0020u32).to_le_bytes());

    bytes[FILE_ALIGNMENT as usize] = 0xC3;
    bytes
}

fn sample_windows_executable_bytes() -> Vec<u8> {
    sample_windows_pe_bytes(false)
}

fn sample_windows_dll_bytes() -> Vec<u8> {
    sample_windows_pe_bytes(true)
}

fn sample_linux_elf_bytes() -> Vec<u8> {
    let mut bytes = vec![0u8; 64];
    bytes[0..4].copy_from_slice(b"\x7FELF");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[6] = 1;
    bytes[16..18].copy_from_slice(&(2u16).to_le_bytes());
    bytes[18..20].copy_from_slice(&(62u16).to_le_bytes());
    bytes[20..24].copy_from_slice(&(1u32).to_le_bytes());
    bytes[24..32].copy_from_slice(&(0x400000u64).to_le_bytes());
    bytes[52..54].copy_from_slice(&(64u16).to_le_bytes());
    bytes
}

#[cfg(unix)]
fn write_linux_elf(path: &Path, mode: u32) -> Result<()> {
    fs::write(path, sample_linux_elf_bytes())?;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_linux_elf(path: &Path, _mode: u32) -> Result<()> {
    fs::write(path, sample_linux_elf_bytes())?;
    Ok(())
}

fn sample_native_build_plan(root: &Path, mode: u32) -> Result<NativeBuildPlan> {
    let manifest_dir = root.join("native-build-project");
    let source_app_path = manifest_dir.join("MyApp.app");
    let binary_path = source_app_path.join("Contents/MacOS/MyApp");
    fs::create_dir_all(binary_path.parent().context("binary parent missing")?)?;
    fs::write(
        manifest_dir.join("capsule.toml"),
        r#"schema_version = "0.2"
name = "my-app"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "native"
entrypoint = "MyApp.app"
"#,
    )?;
    fs::write(&binary_path, b"unsigned-app")?;
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(&binary_path)?.permissions();
        permissions.set_mode(mode);
        fs::set_permissions(&binary_path, permissions)?;
    }

    detect_build_strategy(&manifest_dir)?.context("expected native delivery build plan")
}

fn sample_file_native_build_plan(root: &Path) -> Result<NativeBuildPlan> {
    let manifest_dir = root.join("native-file-build-project");
    let source_file_path = manifest_dir.join("dist/MyApp.exe");
    fs::create_dir_all(
        source_file_path
            .parent()
            .context("source file parent missing")?,
    )?;
    fs::write(
        manifest_dir.join("capsule.toml"),
        r#"schema_version = "0.2"
name = "my-app"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "native"
entrypoint = "dist/MyApp.exe"
"#,
    )?;
    fs::write(&source_file_path, sample_windows_executable_bytes())?;

    detect_build_strategy(&manifest_dir)?.context("expected native delivery build plan")
}

#[test]
fn build_environment_skeleton_captures_native_delivery_inputs() -> Result<()> {
    let tmp = tempdir()?;
    let plan = sample_native_build_plan(tmp.path(), 0o755)?;
    fs::write(plan.workspace_root.join("Cargo.lock"), "version = 3\n")?;
    fs::write(plan.workspace_root.join("package-lock.json"), "{}")?;

    let skeleton = native_delivery_build_environment_skeleton(&plan);
    assert_json_object_has_keys(
        &skeleton,
        &["toolchains", "package_managers", "sdks", "helper_tools"],
    );

    let toolchains = skeleton
        .get("toolchains")
        .and_then(serde_json::Value::as_array)
        .expect("toolchains");
    assert!(toolchains
        .iter()
        .any(|value| value.as_str() == Some("rust")));
    assert!(toolchains
        .iter()
        .any(|value| value.as_str() == Some("cargo")));
    assert!(toolchains
        .iter()
        .any(|value| value.as_str() == Some("node")));

    let package_managers = skeleton
        .get("package_managers")
        .and_then(serde_json::Value::as_array)
        .expect("package_managers");
    assert!(package_managers
        .iter()
        .any(|value| value.as_str() == Some("cargo")));
    assert!(package_managers
        .iter()
        .any(|value| value.as_str() == Some("npm")));

    let sdks = skeleton
        .get("sdks")
        .and_then(serde_json::Value::as_array)
        .expect("sdks");
    assert!(sdks.iter().any(|value| value.as_str() == Some("apple-sdk")));

    let helper_tools = skeleton
        .get("helper_tools")
        .and_then(serde_json::Value::as_array)
        .expect("helper_tools");
    assert!(helper_tools
        .iter()
        .any(|value| value.as_str() == Some("tauri-cli")));
    assert!(helper_tools
        .iter()
        .any(|value| value.as_str() == Some("codesign")));
    Ok(())
}

#[test]
fn finalize_helper_boundary_is_host_local_but_recorded_as_build_environment_claim() {
    let boundary = finalize_helper_boundary("codesign");
    assert_eq!(
        boundary.authority_kind,
        BootstrapAuthorityKind::HostCapability
    );
    assert_eq!(
        boundary.closure_role,
        BootstrapClosureRole::BuildEnvironmentClaim
    );
    assert_eq!(boundary.subject_name, "codesign");
}

#[test]
fn detect_build_strategy_rejects_command_mode_source_delivery_sidecar() -> Result<()> {
    let tmp = tempdir()?;
    let manifest_dir = tmp.path().join("command-build-project");
    fs::create_dir_all(&manifest_dir)?;
    fs::write(
        manifest_dir.join("capsule.toml"),
        r#"schema_version = "0.2"
name = "my-app"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "native"
entrypoint = "sh"
cmd = ["build-app.sh"]
working_dir = "."
"#,
    )?;
    fs::write(
        manifest_dir.join(DELIVERY_CONFIG_FILE),
        sample_delivery_toml(),
    )?;

    let err = detect_build_strategy(&manifest_dir).expect_err("source sidecar must be rejected");
    assert!(err
        .to_string()
        .contains("is no longer accepted in source projects"));
    Ok(())
}

#[test]
fn detect_build_strategy_accepts_windows_exe_manifest_contract() -> Result<()> {
    let tmp = tempdir()?;
    let manifest_dir = tmp.path().join("windows-build-project");
    let source_file_path = manifest_dir.join("dist/MyApp.exe");
    fs::create_dir_all(
        source_file_path
            .parent()
            .context("source file parent missing")?,
    )?;
    fs::write(
        manifest_dir.join("capsule.toml"),
        r#"schema_version = "0.2"
name = "my-app"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "native"
entrypoint = "dist/MyApp.exe"
"#,
    )?;
    fs::write(&source_file_path, sample_windows_executable_bytes())?;

    let plan =
        detect_build_strategy(&manifest_dir)?.context("expected native delivery build plan")?;
    let config = staged_delivery_config(&plan)?;
    assert_eq!(plan.source_app_path, source_file_path);
    assert_eq!(config.artifact.input, "dist/MyApp.exe");
    assert_eq!(
        config.artifact.target,
        format!(
            "windows/{}",
            normalize_delivery_arch(std::env::consts::ARCH)
        )
    );
    Ok(())
}

#[test]
fn detect_build_strategy_ignores_command_mode_without_inline_delivery_config() -> Result<()> {
    let tmp = tempdir()?;
    let manifest_dir = tmp.path().join("command-build-project");
    fs::create_dir_all(&manifest_dir)?;
    fs::write(
        manifest_dir.join("capsule.toml"),
        r#"schema_version = "0.2"
name = "my-app"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "native"
entrypoint = "sh"
cmd = ["build-app.sh"]
working_dir = "."
"#,
    )?;

    assert!(detect_build_strategy(&manifest_dir)?.is_none());
    Ok(())
}

#[test]
fn detect_build_strategy_accepts_inline_delivery_config() -> Result<()> {
    let tmp = tempdir()?;
    let manifest_dir = tmp.path().join("inline-command-build-project");
    fs::create_dir_all(&manifest_dir)?;
    fs::write(
        manifest_dir.join("capsule.toml"),
        r#"schema_version = "0.2"
name = "time-management-desktop"
version = "0.1.0"
type = "app"
default_target = "desktop"

[targets.desktop]
runtime = "source"
driver = "native"
entrypoint = "sh"
cmd = ["build-app.sh"]
working_dir = "."

[artifact]
framework = "tauri"
stage = "unsigned"
target = "darwin/arm64"
input = "dist/time-management-desktop.app"

[finalize]
tool = "codesign"
args = ["--deep", "--force", "--sign", "-", "dist/time-management-desktop.app"]
"#,
    )?;

    let plan =
        detect_build_strategy(&manifest_dir)?.context("expected native delivery build plan")?;
    let build_command = plan.build_command.context("expected build command")?;
    assert_eq!(build_command.program, "sh");
    assert_eq!(build_command.args, vec!["build-app.sh".to_string()]);
    assert_eq!(
        plan.source_app_path,
        manifest_dir.join("dist/time-management-desktop.app")
    );
    Ok(())
}

#[test]
fn detect_build_strategy_generates_canonical_delivery_config_from_capsule_manifest() -> Result<()> {
    let tmp = tempdir()?;
    let manifest_dir = tmp.path().join("native-build-project");
    let source_app_path = manifest_dir.join("MyApp.app");
    let binary_path = source_app_path.join("Contents/MacOS/MyApp");
    fs::create_dir_all(binary_path.parent().context("binary parent missing")?)?;
    fs::write(
        manifest_dir.join("capsule.toml"),
        r#"schema_version = "0.2"
name = "my-app"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "native"
entrypoint = "MyApp.app"
"#,
    )?;
    fs::write(&binary_path, b"unsigned-app")?;
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(&binary_path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&binary_path, permissions)?;
    }

    let plan =
        detect_build_strategy(&manifest_dir)?.context("expected native delivery build plan")?;
    let staged: DeliveryConfig = toml::from_str(&plan.staged_delivery_config_toml)?;

    assert!(plan.delivery_config_path.is_none());
    assert_eq!(plan.source_app_path, manifest_dir.join("MyApp.app"));
    assert_eq!(staged.schema_version, DELIVERY_SCHEMA_VERSION_STABLE);
    assert_eq!(staged.artifact.input, "MyApp.app");
    assert_eq!(staged.artifact.framework, DEFAULT_DELIVERY_FRAMEWORK);
    assert_eq!(staged.artifact.target, DEFAULT_DELIVERY_TARGET);
    assert_eq!(staged.finalize.tool, DEFAULT_FINALIZE_TOOL);
    assert_eq!(
        staged.finalize.args,
        vec![
            "--deep".to_string(),
            "--force".to_string(),
            "--sign".to_string(),
            "-".to_string(),
            "MyApp.app".to_string(),
        ]
    );
    Ok(())
}

#[test]
fn detect_build_strategy_rejects_source_delivery_sidecar_for_canonical_app_targets() {
    let tmp = tempdir().expect("tmp dir");
    let manifest_dir = tmp.path().join("native-build-project");
    let source_app_path = manifest_dir.join("MyApp.app");
    let binary_path = source_app_path.join("Contents/MacOS/MyApp");
    fs::create_dir_all(binary_path.parent().expect("binary parent")).expect("create app");
    fs::write(
        manifest_dir.join("capsule.toml"),
        r#"schema_version = "0.2"
name = "my-app"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "native"
entrypoint = "MyApp.app"
"#,
    )
    .expect("write manifest");
    fs::write(
        manifest_dir.join(DELIVERY_CONFIG_FILE),
        r#"schema_version = "0.1"
[artifact]
framework = "tauri"
stage = "unsigned"
target = "darwin/arm64"
input = "Other.app"
[finalize]
tool = "codesign"
args = ["--deep", "--force", "--sign", "-", "Other.app"]
"#,
    )
    .expect("write sidecar");
    fs::write(&binary_path, b"unsigned-app").expect("write binary");
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(&binary_path)
            .expect("binary metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&binary_path, permissions).expect("set permissions");
    }

    let err = detect_build_strategy(&manifest_dir).expect_err("source sidecar must be rejected");
    assert!(err
        .to_string()
        .contains("is no longer accepted in source projects"));
}

#[test]
fn detect_build_strategy_rejects_partial_inline_delivery_config() {
    let tmp = tempdir().expect("tmp dir");
    let manifest_dir = tmp.path().join("inline-command-build-project");
    fs::create_dir_all(&manifest_dir).expect("create manifest dir");
    fs::write(
        manifest_dir.join("capsule.toml"),
        r#"schema_version = "0.2"
name = "time-management-desktop"
version = "0.1.0"
type = "app"
default_target = "desktop"

[targets.desktop]
runtime = "source"
driver = "native"
entrypoint = "sh"
cmd = ["build-app.sh"]

[artifact]
framework = "tauri"
stage = "unsigned"
target = "darwin/arm64"
input = "dist/time-management-desktop.app"
"#,
    )
    .expect("write manifest");

    let err =
        detect_build_strategy(&manifest_dir).expect_err("should reject partial inline config");
    assert!(err
        .to_string()
        .contains("defines [artifact] without [finalize]"));
}

#[test]
fn validate_native_bundle_directory_reports_nearby_candidates() -> Result<()> {
    let tmp = tempdir()?;
    let macos_dir = tmp.path().join("src-tauri/target/release/bundle/macos");
    let candidate = macos_dir.join("Time Management Desktop.app");
    fs::create_dir_all(&candidate)?;

    let err = validate_native_bundle_directory(&macos_dir.join("time-management-desktop.app"))
        .expect_err("missing exact app path should fail");
    let message = err.to_string();
    assert!(message.contains("Found nearby .app bundle candidates"));
    assert!(message.contains("Time Management Desktop.app"));
    Ok(())
}

#[test]
fn validate_native_bundle_directory_reports_nearby_exe_candidates() -> Result<()> {
    let tmp = tempdir()?;
    let windows_dir = tmp.path().join("src-tauri/target/release/bundle/windows");
    let candidate = windows_dir.join("Time Management Desktop.exe");
    fs::create_dir_all(&windows_dir)?;
    fs::write(&candidate, sample_windows_executable_bytes())?;

    let err = validate_native_bundle_directory(&windows_dir.join("time-management-desktop.exe"))
        .expect_err("missing exact exe path should fail");
    let message = err.to_string();
    assert!(message.contains("Found nearby .exe candidates"));
    assert!(message.contains("Time Management Desktop.exe"));
    Ok(())
}

#[test]
fn validate_native_bundle_directory_reports_nearby_appimage_candidates() -> Result<()> {
    let tmp = tempdir()?;
    let linux_dir = tmp.path().join("src-tauri/target/release/bundle/appimage");
    let candidate = linux_dir.join("Time Management Desktop.AppImage");
    fs::create_dir_all(&linux_dir)?;
    write_linux_elf(&candidate, 0o755)?;

    let err = validate_native_bundle_directory(&linux_dir.join("time-management-desktop.AppImage"))
        .expect_err("missing exact AppImage path should fail");
    let message = err.to_string();
    assert!(message.contains("Found nearby .AppImage candidates"));
    assert!(message.contains("Time Management Desktop.AppImage"));
    Ok(())
}

#[test]
fn validate_native_bundle_directory_accepts_linux_directory_and_files() -> Result<()> {
    let tmp = tempdir()?;
    let linux_dir = tmp.path().join("dist/linux");
    let linux_binary = linux_dir.join("MyApp");
    let linux_deb = tmp.path().join("dist/MyApp.deb");
    let windows_exe = tmp.path().join("dist/MyApp.exe");
    fs::create_dir_all(&linux_dir)?;
    fs::create_dir_all(windows_exe.parent().context("missing exe parent")?)?;
    write_linux_elf(&linux_binary, 0o755)?;
    fs::write(&linux_deb, b"!<arch>\n")?;
    fs::write(&windows_exe, sample_windows_executable_bytes())?;

    validate_native_bundle_directory(&linux_dir)?;
    validate_native_bundle_directory(&linux_deb)?;
    validate_native_bundle_directory(&windows_exe)?;
    Ok(())
}

#[test]
fn validate_native_bundle_directory_rejects_invalid_linux_elf() -> Result<()> {
    let tmp = tempdir()?;
    let linux_file = tmp.path().join("dist/MyApp.AppImage");
    fs::create_dir_all(linux_file.parent().context("missing linux parent")?)?;
    fs::write(&linux_file, b"\x7FELFnot-a-valid-elf")?;
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(&linux_file)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&linux_file, permissions)?;
    }

    let err = validate_native_bundle_directory(&linux_file)
        .expect_err("invalid AppImage should fail ELF validation");
    assert!(err
        .to_string()
        .contains("Linux executable failed minimum ELF validation"));
    Ok(())
}

#[test]
fn validate_native_bundle_directory_rejects_linux_directory_without_elf_executable() -> Result<()> {
    let tmp = tempdir()?;
    let linux_dir = tmp.path().join("dist/linux");
    let launcher = linux_dir.join("AppRun");
    fs::create_dir_all(&linux_dir)?;
    fs::write(&launcher, b"#!/bin/sh\nexit 0\n")?;
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(&launcher)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&launcher, permissions)?;
    }

    let err = validate_native_bundle_directory(&linux_dir)
        .expect_err("directory without an ELF executable should fail closed");
    assert!(err.to_string().contains("missing a regular ELF executable"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn validate_native_bundle_directory_rejects_linux_elf_without_executable_bit() -> Result<()> {
    let tmp = tempdir()?;
    let linux_file = tmp.path().join("dist/MyApp");
    fs::create_dir_all(linux_file.parent().context("missing linux parent")?)?;
    write_linux_elf(&linux_file, 0o644)?;

    let err = validate_native_bundle_directory(&linux_file)
        .expect_err("missing executable bit should fail closed");
    assert!(err.to_string().contains("Executable bit is missing"));
    Ok(())
}

#[test]
fn validate_native_bundle_directory_rejects_invalid_windows_executable() -> Result<()> {
    let tmp = tempdir()?;
    let windows_exe = tmp.path().join("dist/MyApp.exe");
    fs::create_dir_all(windows_exe.parent().context("missing exe parent")?)?;
    fs::write(&windows_exe, b"not-a-pe-file")?;

    let err = validate_native_bundle_directory(&windows_exe)
        .expect_err("invalid exe should fail PE validation");
    assert!(err
        .to_string()
        .contains("Windows executable failed minimum PE validation"));
    Ok(())
}

#[test]
fn validate_native_bundle_directory_rejects_windows_dll_renamed_to_exe() -> Result<()> {
    let tmp = tempdir()?;
    let windows_exe = tmp.path().join("dist/MyApp.exe");
    fs::create_dir_all(windows_exe.parent().context("missing exe parent")?)?;
    fs::write(&windows_exe, sample_windows_dll_bytes())?;

    let err = validate_native_bundle_directory(&windows_exe)
        .expect_err("dll-shaped PE should fail executable validation");
    assert!(err.to_string().contains("is a DLL, not an .exe"));
    Ok(())
}

#[test]
fn build_accepts_windows_single_file_native_artifacts() -> Result<()> {
    let tmp = tempdir()?;
    let plan = sample_file_native_build_plan(tmp.path())?;
    let artifact_path = tmp.path().join("out/my-app-0.1.0.capsule");

    let result =
        build_native_artifact_with_strip(&plan, Some(&artifact_path), |_path| Ok(()), None)?;

    assert_eq!(result.artifact_path, artifact_path);
    assert_eq!(result.derived_from, plan.source_app_path);
    let entry_modes = read_payload_entry_modes(&result.artifact_path)?;
    assert!(entry_modes.contains_key("dist/MyApp.exe"));
    Ok(())
}

fn read_payload_entry_modes(artifact_path: &Path) -> Result<BTreeMap<String, u32>> {
    let capsule_bytes = fs::read(artifact_path)?;
    let mut capsule = tar::Archive::new(Cursor::new(capsule_bytes));
    let mut payload_tar_zst = None;
    for entry in capsule.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        if path == Path::new("payload.tar.zst") {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            payload_tar_zst = Some(bytes);
            break;
        }
    }

    let payload_tar_zst = payload_tar_zst.context("payload.tar.zst missing from capsule")?;
    let payload_tar = zstd::stream::decode_all(Cursor::new(payload_tar_zst))?;
    let mut payload = tar::Archive::new(Cursor::new(payload_tar));
    let mut entry_modes = BTreeMap::new();
    for entry in payload.entries()? {
        let entry = entry?;
        let path = entry.path()?.display().to_string();
        entry_modes.insert(path, entry.header().mode()?);
    }
    Ok(entry_modes)
}

fn read_capsule_manifest_value(artifact_path: &Path) -> Result<toml::Value> {
    let capsule_bytes = fs::read(artifact_path)?;
    let mut capsule = tar::Archive::new(Cursor::new(capsule_bytes));
    for entry in capsule.entries()? {
        let mut entry = entry?;
        if entry.path()?.as_ref() == Path::new("capsule.toml") {
            let mut raw = String::new();
            entry.read_to_string(&mut raw)?;
            return toml::from_str(&raw).map_err(anyhow::Error::from);
        }
    }
    bail!("capsule.toml missing from capsule")
}

fn sample_fetch_dir_with_mode(root: &Path, mode: u32) -> Result<PathBuf> {
    let fetched_dir = root.join("fetched");
    let artifact_dir = fetched_dir.join(FETCH_ARTIFACT_DIR);
    fs::create_dir_all(artifact_dir.join("MyApp.app/Contents/MacOS"))?;
    fs::write(
        artifact_dir.join(DELIVERY_CONFIG_FILE),
        sample_delivery_toml(),
    )?;
    fs::write(
        artifact_dir.join("MyApp.app/Contents/MacOS/MyApp"),
        b"unsigned-app",
    )?;
    #[cfg(unix)]
    {
        let binary = artifact_dir.join("MyApp.app/Contents/MacOS/MyApp");
        let mut permissions = fs::metadata(&binary)?.permissions();
        permissions.set_mode(mode);
        fs::set_permissions(&binary, permissions)?;
    }
    let metadata = FetchMetadata {
        schema_version: DELIVERY_SCHEMA_VERSION.to_string(),
        scoped_id: "local/my-app".to_string(),
        version: "0.1.0".to_string(),
        registry: "http://127.0.0.1:8787".to_string(),
        fetched_at: "2026-03-09T00:00:00Z".to_string(),
        parent_digest: compute_tree_digest(&artifact_dir)?,
        artifact_blake3: compute_blake3(b"artifact"),
    };
    fs::create_dir_all(&fetched_dir)?;
    write_json_pretty(&fetched_dir.join(FETCH_METADATA_FILE), &metadata)?;
    Ok(fetched_dir)
}

fn sample_file_fetch_dir(root: &Path) -> Result<PathBuf> {
    let fetched_dir = root.join("fetched-file");
    let artifact_dir = fetched_dir.join(FETCH_ARTIFACT_DIR);
    fs::create_dir_all(artifact_dir.join("dist"))?;
    fs::write(
        artifact_dir.join(DELIVERY_CONFIG_FILE),
        sample_file_delivery_toml(),
    )?;
    fs::write(
        artifact_dir.join("dist/MyApp.exe"),
        sample_windows_executable_bytes(),
    )?;
    let metadata = FetchMetadata {
        schema_version: DELIVERY_SCHEMA_VERSION.to_string(),
        scoped_id: "local/my-app".to_string(),
        version: "0.1.0".to_string(),
        registry: "http://127.0.0.1:8787".to_string(),
        fetched_at: "2026-03-09T00:00:00Z".to_string(),
        parent_digest: compute_tree_digest(&artifact_dir)?,
        artifact_blake3: compute_blake3(b"artifact"),
    };
    fs::create_dir_all(&fetched_dir)?;
    write_json_pretty(&fetched_dir.join(FETCH_METADATA_FILE), &metadata)?;
    Ok(fetched_dir)
}

fn sample_nested_fetch_dir(root: &Path) -> Result<PathBuf> {
    let fetched_dir = root.join("fetched-nested");
    let artifact_dir = fetched_dir.join(FETCH_ARTIFACT_DIR);
    let app_dir = artifact_dir.join("src-tauri/target/release/bundle/macos/My App.app");
    fs::create_dir_all(app_dir.join("Contents/MacOS"))?;
    fs::write(
        artifact_dir.join(DELIVERY_CONFIG_FILE),
        sample_nested_delivery_toml(),
    )?;
    fs::write(app_dir.join("Contents/MacOS/My App"), b"unsigned-app")?;
    #[cfg(unix)]
    {
        let binary = app_dir.join("Contents/MacOS/My App");
        let mut permissions = fs::metadata(&binary)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&binary, permissions)?;
    }
    let metadata = FetchMetadata {
        schema_version: DELIVERY_SCHEMA_VERSION.to_string(),
        scoped_id: "local/my-app".to_string(),
        version: "0.1.0".to_string(),
        registry: "http://127.0.0.1:8787".to_string(),
        fetched_at: "2026-03-09T00:00:00Z".to_string(),
        parent_digest: compute_tree_digest(&artifact_dir)?,
        artifact_blake3: compute_blake3(b"artifact"),
    };
    fs::create_dir_all(&fetched_dir)?;
    write_json_pretty(&fetched_dir.join(FETCH_METADATA_FILE), &metadata)?;
    Ok(fetched_dir)
}

fn sample_finalized_app(root: &Path) -> Result<(PathBuf, PathBuf)> {
    sample_finalized_app_with_target(root, sample_supported_projection_target())
}

fn sample_finalized_app_with_target(root: &Path, target: &str) -> Result<(PathBuf, PathBuf)> {
    let derived_dir = root.join("derived-output");
    let derived_app = if delivery_target_os_family(target) == Some("linux") {
        let derived_app = derived_dir.join("my-app");
        let binary = derived_app.join("my-app");
        fs::create_dir_all(&derived_app)?;
        fs::write(&binary, b"#!/bin/sh\necho signed-app\n")?;
        #[cfg(unix)]
        {
            let mut permissions = fs::metadata(&binary)?.permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&binary, permissions)?;
        }
        derived_app
    } else if delivery_target_os_family(target) == Some("windows") {
        let derived_app = derived_dir.join("MyApp");
        let binary = derived_app.join("MyApp.exe");
        fs::create_dir_all(&derived_app)?;
        fs::write(&binary, b"signed-app")?;
        #[cfg(unix)]
        {
            let mut permissions = fs::metadata(&binary)?.permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&binary, permissions)?;
        }
        derived_app
    } else {
        let derived_app = derived_dir.join("MyApp.app");
        fs::create_dir_all(derived_app.join("Contents/MacOS"))?;
        fs::write(derived_app.join("Contents/MacOS/MyApp"), b"signed-app")?;
        #[cfg(unix)]
        {
            let binary = derived_app.join("Contents/MacOS/MyApp");
            let mut permissions = fs::metadata(&binary)?.permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&binary, permissions)?;
        }
        derived_app
    };
    let provenance = LocalDerivationProvenance {
        schema_version: DELIVERY_SCHEMA_VERSION.to_string(),
        scoped_id: None,
        version: None,
        registry: None,
        artifact_blake3: None,
        parent_digest: "blake3:parent-digest".to_string(),
        derived_digest: compute_tree_digest(&derived_app)?,
        framework: DEFAULT_DELIVERY_FRAMEWORK.to_string(),
        target: target.to_string(),
        finalized_locally: true,
        finalize_tool: DEFAULT_FINALIZE_TOOL.to_string(),
        finalized_at: "2026-03-09T00:00:00Z".to_string(),
    };
    write_json_pretty(&derived_dir.join(PROVENANCE_FILE), &provenance)?;
    Ok((derived_dir, derived_app))
}

fn sample_supported_projection_target() -> &'static str {
    match host_projection_os_family() {
        Some("linux") => "linux/x86_64",
        Some("windows") => "windows/x86_64",
        _ => "darwin/arm64",
    }
}

fn sample_projection_launcher_dir(root: &Path) -> PathBuf {
    root.join("launcher")
}

fn sample_projection_command_dir(root: &Path) -> PathBuf {
    root.join("bin")
}

fn sample_projection_binary_path(derived_app: &Path) -> PathBuf {
    if path_has_extension(derived_app, "app") {
        derived_app.join("Contents/MacOS/MyApp")
    } else {
        let windows_binary = derived_app.join("MyApp.exe");
        if windows_binary.exists() {
            windows_binary
        } else {
            derived_app.join("my-app")
        }
    }
}

#[test]
fn sanitize_projection_segment_normalizes_special_characters() {
    assert_eq!(sanitize_projection_segment("My App"), "my-app");
    assert_eq!(sanitize_projection_segment("---"), "ato-app");
    assert_eq!(sanitize_projection_segment("my___app"), "my___app");
    assert_eq!(sanitize_projection_segment("My.App"), "my-app");
    assert_eq!(sanitize_projection_segment("My...App"), "my-app");
}

#[test]
fn projection_name_helpers_prefer_scoped_slug_when_available() -> Result<()> {
    let derived_app_path = Path::new("Time Management Desktop.app");
    assert_eq!(
        projection_display_name(derived_app_path, Some("koh0920/time-management-desktop"))?,
        "Time Management Desktop"
    );
    assert_eq!(
        projection_command_name(derived_app_path, Some("koh0920/time-management-desktop"))?,
        "time-management-desktop"
    );
    Ok(())
}

#[test]
fn render_linux_desktop_entry_escapes_special_characters() {
    let rendered = render_linux_desktop_entry(
        "My App\nTabbed\tName",
        Path::new("My App/bin/my\"app"),
        Path::new("My App/root"),
    );
    assert!(rendered.contains("Name=My App\\nTabbed\\tName"));
    assert!(rendered.contains("Exec=My\\ App/bin/my\\\"app"));
    assert!(rendered.contains("Path=My App/root"));
}

#[test]
fn resolve_linux_projection_command_target_prefers_named_binary() -> Result<()> {
    let tmp = tempdir()?;
    let app_dir = tmp.path().join("my-app");
    let preferred = app_dir.join("my-app");
    let other = app_dir.join("bin/helper");
    fs::create_dir_all(other.parent().context("helper parent missing")?)?;
    fs::write(&preferred, b"#!/bin/sh\n")?;
    fs::write(&other, b"#!/bin/sh\n")?;
    #[cfg(unix)]
    {
        for path in [&preferred, &other] {
            let mut permissions = fs::metadata(path)?.permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions)?;
        }
    }

    assert_eq!(
        resolve_linux_projection_command_target(&app_dir)?,
        preferred
    );
    Ok(())
}

#[test]
fn resolve_linux_projection_command_target_rejects_multiple_candidates() -> Result<()> {
    let tmp = tempdir()?;
    let app_dir = tmp.path().join("my-app");
    let first = app_dir.join("bin/alpha");
    let second = app_dir.join("bin/beta");
    fs::create_dir_all(first.parent().context("bin parent missing")?)?;
    fs::write(&first, b"#!/bin/sh\n")?;
    fs::write(&second, b"#!/bin/sh\n")?;
    #[cfg(unix)]
    {
        for path in [&first, &second] {
            let mut permissions = fs::metadata(path)?.permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions)?;
        }
    }

    let err = resolve_linux_projection_command_target(&app_dir)
        .expect_err("multiple executable candidates should fail");
    assert!(err
        .to_string()
        .contains("multiple executable command candidates"));
    Ok(())
}

#[test]
fn delivery_config_accepts_non_codesign_tool_and_non_default_target() {
    let config: DeliveryConfig = toml::from_str(
        r#"schema_version = "0.1"
[artifact]
    framework = "tauri"
    stage = "unsigned"
    target = "windows/x86_64"
    input = "MyApp.app"
[finalize]
    tool = "signtool"
    args = ["sign", "/fd", "SHA256", "MyApp.app"]
"#,
    )
    .expect("config parse");
    validate_delivery_config(&config).expect("config should be accepted");
}

#[test]
fn delivery_config_accepts_signtool_with_timestamp_args() {
    let config: DeliveryConfig = toml::from_str(
        r#"schema_version = "0.1"
[artifact]
    framework = "tauri"
    stage = "unsigned"
    target = "windows/x86_64"
    input = "dist/MyApp.exe"
[finalize]
    tool = "signtool"
    args = ["sign", "/fd", "SHA256", "/tr", "http://tsa.test", "/td", "SHA256", "dist/MyApp.exe"]
"#,
    )
    .expect("config parse");
    validate_delivery_config(&config).expect("config should be accepted");
}

#[test]
fn delivery_config_rejects_unknown_signtool_switch() {
    let config: DeliveryConfig = toml::from_str(
        r#"schema_version = "0.1"
[artifact]
    framework = "tauri"
    stage = "unsigned"
    target = "windows/x86_64"
    input = "dist/MyApp.exe"
[finalize]
    tool = "signtool"
    args = ["sign", "/bogus", "dist/MyApp.exe"]
"#,
    )
    .expect("config parse");
    let err = validate_delivery_config(&config).expect_err("config should be rejected");
    assert!(err
        .to_string()
        .contains("Unsupported finalize.args entry '/bogus'"));
}

#[test]
fn delivery_config_rejects_unsupported_target() {
    let config: DeliveryConfig = toml::from_str(
        r#"schema_version = "0.1"
[artifact]
    framework = "tauri"
    stage = "unsigned"
    target = "solaris/x86_64"
    input = "MyApp.app"
[finalize]
    tool = "codesign"
    args = ["--deep", "--force", "--sign", "-", "MyApp.app"]
"#,
    )
    .expect("config parse");
    let err = validate_delivery_config(&config).expect_err("config should be rejected");
    assert!(err
        .to_string()
        .contains("Unsupported artifact.target 'solaris/x86_64'"));
}

#[test]
fn delivery_config_accepts_linux_target_with_noop_finalize_tool() {
    let config: DeliveryConfig = toml::from_str(
        r#"schema_version = "0.1"
[artifact]
    framework = "tauri"
    stage = "unsigned"
    target = "linux/x86_64"
    input = "dist/my-app"
[finalize]
    tool = "none"
    args = []
"#,
    )
    .expect("config parse");
    validate_delivery_config(&config).expect("config should be accepted");
}

#[test]
fn delivery_config_accepts_linux_aarch64_with_chmod_finalize_tool() {
    let config: DeliveryConfig = toml::from_str(
        r#"schema_version = "0.1"
[artifact]
    framework = "tauri"
    stage = "unsigned"
    target = "linux/aarch64"
    input = "dist/my-app"
[finalize]
    tool = "chmod"
    args = ["0755", "dist/my-app"]
"#,
    )
    .expect("config parse");
    validate_delivery_config(&config).expect("config should be accepted");
}

#[test]
fn resolve_fetch_request_accepts_issue_style_inline_registry_ref() -> Result<()> {
    let resolved = resolve_fetch_request("localhost:8080/my-tauri-app:unsigned-0.1.0", None, None)?;
    assert_eq!(
        resolved,
        ResolvedFetchRequest {
            capsule_ref: "local/my-tauri-app".to_string(),
            registry_url: Some("http://localhost:8080".to_string()),
            version: Some("unsigned-0.1.0".to_string()),
        }
    );
    Ok(())
}

#[test]
fn resolve_fetch_request_accepts_inline_registry_with_explicit_scope() -> Result<()> {
    let resolved = resolve_fetch_request(
        "https://127.0.0.1:8787/koh0920/sample-native-capsule:0.1.0",
        None,
        None,
    )?;
    assert_eq!(
        resolved,
        ResolvedFetchRequest {
            capsule_ref: "koh0920/sample-native-capsule".to_string(),
            registry_url: Some("https://127.0.0.1:8787".to_string()),
            version: Some("0.1.0".to_string()),
        }
    );
    Ok(())
}

#[test]
fn resolve_fetch_request_rejects_conflicting_registry_override() {
    let err = resolve_fetch_request(
        "localhost:8080/my-tauri-app:unsigned-0.1.0",
        Some("http://127.0.0.1:8787"),
        None,
    )
    .expect_err("registry conflict must fail");
    assert!(err.to_string().contains("conflicting_registry_request"));
}

#[test]
fn tree_digest_is_stable_for_identical_trees() -> Result<()> {
    let tmp = tempdir()?;
    let left = tmp.path().join("left");
    let right = tmp.path().join("right");
    fs::create_dir_all(left.join("a/b"))?;
    fs::create_dir_all(right.join("a/b"))?;
    fs::write(left.join("a/b/file.txt"), b"hello")?;
    fs::write(right.join("a/b/file.txt"), b"hello")?;
    assert_eq!(compute_tree_digest(&left)?, compute_tree_digest(&right)?);
    Ok(())
}

#[test]
fn native_delivery_documented_json_contract_fields_are_present() -> Result<()> {
    let tmp = tempdir()?;

    let fetched_dir = sample_fetch_dir(tmp.path())?;
    let fetch_metadata = load_fetch_metadata(&fetched_dir)?;
    let fetch_json = serde_json::to_value(&fetch_metadata)?;
    assert_json_object_has_keys(
        &fetch_json,
        &[
            "schema_version",
            "scoped_id",
            "version",
            "registry",
            "parent_digest",
        ],
    );

    let build_json = serde_json::to_value(NativeBuildResult {
        artifact_path: tmp.path().join("out/my-app-0.1.0.capsule"),
        build_strategy: "native-delivery".to_string(),
        target: DEFAULT_DELIVERY_TARGET.to_string(),
        derived_from: tmp.path().join("MyApp.app"),
        schema_version: DELIVERY_SCHEMA_VERSION.to_string(),
    })?;
    assert_json_object_has_keys(
        &build_json,
        &["build_strategy", "schema_version", "target", "derived_from"],
    );

    let (derived_dir, derived_app) = sample_finalized_app(tmp.path())?;
    let provenance_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(derived_dir.join(PROVENANCE_FILE))?)?;
    assert_json_object_has_keys(
        &provenance_json,
        &[
            "schema_version",
            "parent_digest",
            "derived_digest",
            "framework",
            "target",
            "finalize_tool",
            "finalized_at",
        ],
    );

    let finalize_json = serde_json::to_value(FinalizeResult {
        fetched_dir: fetched_dir.clone(),
        output_dir: derived_dir.clone(),
        derived_app_path: derived_app.clone(),
        provenance_path: derived_dir.join(PROVENANCE_FILE),
        parent_digest: "blake3:parent-digest".to_string(),
        derived_digest: "blake3:derived-digest".to_string(),
        schema_version: DELIVERY_SCHEMA_VERSION.to_string(),
    })?;
    assert_json_object_has_keys(
        &finalize_json,
        &[
            "schema_version",
            "derived_app_path",
            "provenance_path",
            "parent_digest",
            "derived_digest",
        ],
    );

    let launcher_dir = tmp.path().join("Applications");
    let metadata_root = tmp.path().join("projection-metadata");
    let project_result = project_with_roots(&derived_app, &launcher_dir, &metadata_root)?;
    let project_json = serde_json::to_value(&project_result)?;
    assert_json_object_has_keys(
        &project_json,
        &[
            "schema_version",
            "projection_id",
            "metadata_path",
            "projected_path",
            "derived_app_path",
            "parent_digest",
            "derived_digest",
            "state",
        ],
    );

    let unproject_result =
        unproject_with_metadata_root(&project_result.projection_id, &metadata_root)?;
    let unproject_json = serde_json::to_value(&unproject_result)?;
    assert_json_object_has_keys(
        &unproject_json,
        &[
            "schema_version",
            "projection_id",
            "metadata_path",
            "projected_path",
            "removed_projected_path",
            "removed_metadata",
            "state_before",
        ],
    );

    Ok(())
}

#[test]
fn cargo_native_build_target_dir_uses_manifest_parent_target_dir() {
    let command = NativeBuildCommand {
        program: "cargo".to_string(),
        args: vec![
            "build".to_string(),
            "--manifest-path".to_string(),
            "src-tauri/Cargo.toml".to_string(),
            "--release".to_string(),
        ],
        working_dir: PathBuf::from("/workspace/app"),
    };

    let target_dir = cargo_native_build_target_dir(&command).expect("cargo target dir");

    assert_eq!(target_dir, PathBuf::from("/workspace/app/src-tauri/target"));
}

#[test]
fn cargo_native_build_target_dir_defaults_to_working_dir() {
    let command = NativeBuildCommand {
        program: "cargo.exe".to_string(),
        args: vec!["build".to_string(), "--release".to_string()],
        working_dir: PathBuf::from("/workspace/app/src-tauri"),
    };

    let target_dir = cargo_native_build_target_dir(&command).expect("cargo target dir");

    assert_eq!(target_dir, PathBuf::from("/workspace/app/src-tauri/target"));
}

#[test]
fn configure_native_build_process_rehomes_nested_cargo_outputs() {
    let command = NativeBuildCommand {
        program: "cargo".to_string(),
        args: vec![
            "build".to_string(),
            "--manifest-path=src-tauri/Cargo.toml".to_string(),
            "--release".to_string(),
        ],
        working_dir: PathBuf::from("/workspace/app"),
    };
    let mut process = std::process::Command::new("cargo");

    configure_native_build_process(&mut process, &command);

    let envs = process
        .get_envs()
        .map(|(key, value)| (key.to_os_string(), value.map(|entry| entry.to_os_string())))
        .collect::<Vec<_>>();
    assert!(envs
        .iter()
        .any(|(key, value)| { key == "CARGO_BUILD_TARGET" && value.is_none() }));
    assert!(envs.iter().any(|(key, value)| {
        key == "CARGO_TARGET_DIR"
            && value.as_ref() == Some(&std::ffi::OsString::from("/workspace/app/src-tauri/target"))
    }));
}

#[test]
fn lock_native_build_command_object_accepts_flattened_shape() {
    let build = serde_json::json!({
        "kind": "native-delivery",
        "program": "cargo",
        "args": ["build", "--release"],
        "working_dir": "."
    });

    let command = lock_native_build_command_object(build.as_object().expect("build object"))
        .expect("flattened command object");

    assert_eq!(
        command.get("program").and_then(|value| value.as_str()),
        Some("cargo")
    );
}

#[test]
fn lock_native_build_command_object_accepts_nested_shape() {
    let build = serde_json::json!({
        "kind": "native-delivery",
        "build_command": {
            "program": "cargo",
            "args": ["build", "--release"],
            "working_dir": "."
        }
    });

    let command = lock_native_build_command_object(build.as_object().expect("build object"))
        .expect("nested command object");

    assert_eq!(
        command.get("program").and_then(|value| value.as_str()),
        Some("cargo")
    );
}

#[test]
fn build_native_artifact_preserves_source_and_payload_executable_mode() -> Result<()> {
    let tmp = tempdir()?;
    let plan = sample_native_build_plan(tmp.path(), 0o755)?;
    let source_digest_before = compute_tree_digest(&plan.source_app_path)?;
    let artifact_path = tmp.path().join("out/my-app-0.1.0.capsule");

    let result =
        build_native_artifact_with_strip(&plan, Some(&artifact_path), |_app| Ok(()), None)?;

    assert_eq!(result.build_strategy, "native-delivery");
    assert_eq!(
        result.target,
        default_delivery_target_for_input("MyApp.app")
    );
    assert_eq!(result.derived_from, plan.source_app_path);
    assert_eq!(
        compute_tree_digest(&plan.source_app_path)?,
        source_digest_before
    );

    let entry_modes = read_payload_entry_modes(&artifact_path)?;
    assert!(entry_modes.contains_key(DELIVERY_CONFIG_FILE));
    #[cfg(unix)]
    assert_eq!(
        entry_modes
            .get("MyApp.app/Contents/MacOS/MyApp")
            .copied()
            .unwrap_or_default()
            & 0o111,
        0o111
    );
    let manifest_value = read_capsule_manifest_value(&artifact_path)?;
    assert!(manifest_value
        .get("distribution")
        .and_then(|value| value.as_table())
        .is_some());
    Ok(())
}

#[test]
fn test_build_rejects_non_executable_without_mutation() -> Result<()> {
    let tmp = tempdir()?;
    let plan = sample_native_build_plan(tmp.path(), 0o755)?;
    #[cfg(unix)]
    {
        let binary_path = plan.source_app_path.join("Contents/MacOS/MyApp");
        let mut permissions = fs::metadata(&binary_path)?.permissions();
        permissions.set_mode(0o644);
        fs::set_permissions(&binary_path, permissions)?;
    }
    let source_digest_before = compute_tree_digest(&plan.source_app_path)?;
    let artifact_path = tmp.path().join("out/my-app-0.1.0.capsule");

    let result = build_native_artifact_with_strip(&plan, Some(&artifact_path), |_app| Ok(()), None);

    if cfg!(unix) {
        let err = result.expect_err("build must fail closed when executable bit is missing");
        assert!(err.to_string().contains("Executable bit is missing"));
        assert!(!artifact_path.exists());
    } else {
        let built = result.expect("non-macOS hosts currently skip app permission enforcement");
        assert_eq!(built.artifact_path, artifact_path);
    }
    assert_eq!(
        compute_tree_digest(&plan.source_app_path)?,
        source_digest_before
    );
    Ok(())
}

#[test]
fn finalize_creates_derived_copy_without_mutating_parent() -> Result<()> {
    let tmp = tempdir()?;
    let fetched_dir = sample_fetch_dir(tmp.path())?;
    let artifact_dir = fetched_dir.join(FETCH_ARTIFACT_DIR);
    let parent_before = compute_tree_digest(&artifact_dir)?;
    let output_root = tmp.path().join("dist");

    let result = finalize_with_runner(&fetched_dir, &output_root, |derived_dir, _config| {
        let app_binary = derived_dir.join("MyApp.app/Contents/MacOS/MyApp");
        fs::write(&app_binary, b"signed-app")?;
        Ok(())
    })?;

    assert_eq!(parent_before, result.parent_digest);
    assert_eq!(compute_tree_digest(&artifact_dir)?, parent_before);
    assert!(result.derived_app_path.exists());
    assert!(result.provenance_path.exists());
    assert_ne!(result.parent_digest, result.derived_digest);
    #[cfg(unix)]
    {
        let derived_binary = result.derived_app_path.join("Contents/MacOS/MyApp");
        assert_ne!(
            fs::metadata(&derived_binary)?.permissions().mode() & 0o111,
            0
        );
    }
    let sidecar: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&result.provenance_path)?)?;
    assert_eq!(sidecar["parent_digest"], result.parent_digest);
    assert_eq!(sidecar["derived_digest"], result.derived_digest);
    assert_eq!(sidecar["finalize_tool"], DEFAULT_FINALIZE_TOOL);
    Ok(())
}

#[test]
fn finalize_rejects_missing_executable_bit() -> Result<()> {
    let tmp = tempdir()?;
    let fetched_dir = sample_fetch_dir_with_mode(tmp.path(), 0o644)?;
    let output_root = tmp.path().join("dist");

    let result = finalize_with_runner(&fetched_dir, &output_root, |_derived_dir, _config| Ok(()));
    if cfg!(unix) {
        let err = result.expect_err("finalize must fail closed when executable bit is missing");
        assert!(err.to_string().contains("Executable bit is missing"));
    } else {
        result.expect("non-macOS hosts currently skip app permission enforcement");
    }
    Ok(())
}

#[test]
fn finalize_accepts_windows_single_file_native_artifacts() -> Result<()> {
    let tmp = tempdir()?;
    let fetched_dir = sample_file_fetch_dir(tmp.path())?;
    let output_root = tmp.path().join("dist");

    let result = finalize_with_runner(&fetched_dir, &output_root, |_derived_dir, _config| Ok(()))?;

    assert_eq!(
        result
            .derived_app_path
            .file_name()
            .and_then(|value| value.to_str()),
        Some("MyApp.exe")
    );
    assert!(result.derived_app_path.is_file());
    Ok(())
}

#[test]
fn finalize_rebases_nested_input_to_local_app_name() -> Result<()> {
    let tmp = tempdir()?;
    let fetched_dir = sample_nested_fetch_dir(tmp.path())?;
    let output_root = tmp.path().join("dist");

    let result = finalize_with_runner(&fetched_dir, &output_root, |derived_dir, config| {
        assert_eq!(config.artifact.input, "My App.app");
        assert_eq!(config.finalize.args[4], "My App.app");
        let app_binary = derived_dir.join("My App.app/Contents/MacOS/My App");
        fs::write(&app_binary, b"signed-app")?;
        Ok(())
    })?;

    assert_eq!(
        result
            .derived_app_path
            .file_name()
            .and_then(|value| value.to_str()),
        Some("My App.app")
    );
    Ok(())
}

#[test]
fn rebase_delivery_config_updates_matching_finalize_args() -> Result<()> {
    let tmp = tempdir()?;
    let config: DeliveryConfig = toml::from_str(
        r#"schema_version = "0.1"
[artifact]
    framework = "tauri"
    stage = "unsigned"
    target = "windows/x86_64"
    input = "dist/MyApp.exe"
[finalize]
    tool = "signtool"
    args = ["sign", "/fd", "SHA256", "dist/MyApp.exe", "/tr", "http://tsa.test/dist/MyApp.exe"]
"#,
    )?;
    let rebased = rebase_delivery_config_for_finalize(&config, &tmp.path().join("MyApp.exe"))?;
    assert_eq!(rebased.artifact.input, "MyApp.exe");
    assert_eq!(rebased.finalize.args[3], "MyApp.exe");
    assert_eq!(rebased.finalize.args[5], "http://tsa.test/dist/MyApp.exe");
    Ok(())
}

#[test]
fn delivery_target_os_family_parses_expected_values() {
    assert_eq!(delivery_target_os_family("darwin/arm64"), Some("darwin"));
    assert_eq!(delivery_target_os_family("windows/x86_64"), Some("windows"));
    assert_eq!(delivery_target_os_family(""), None);
    assert_eq!(delivery_target_os_family("/arm64"), None);
}

#[test]
fn supports_projection_target_accepts_supported_platforms() {
    assert!(supports_projection_target("darwin/arm64"));
    assert!(supports_projection_target("darwin/x86_64"));
    assert!(supports_projection_target("linux/x86_64"));
    assert!(supports_projection_target("windows/x86_64"));
    assert!(!supports_projection_target(""));
}

#[test]
fn first_existing_projection_candidate_returns_none_for_missing_paths() -> Result<()> {
    let tmp = tempdir()?;
    let missing = tmp.path().join("Applications").join("MissingApp");
    assert_eq!(first_existing_projection_candidate(&missing)?, None);
    Ok(())
}

#[cfg(windows)]
#[test]
fn windows_shortcut_roundtrip_resolves_expected_target() -> Result<()> {
    let tmp = tempdir()?;
    let target = tmp.path().join("MyApp");
    fs::create_dir_all(&target)?;
    let shortcut = projection_shortcut_path(&tmp.path().join("Launcher").join("MyApp"));
    let shortcut_parent = shortcut
        .parent()
        .ok_or_else(|| anyhow::anyhow!("shortcut path missing parent"))?;
    fs::create_dir_all(shortcut_parent)?;

    create_projection_shortcut(&target, &shortcut)?;

    assert!(shortcut.is_file());
    assert!(is_projection_shortcut(&shortcut, &fs::metadata(&shortcut)?));
    assert!(paths_match(
        &resolve_projection_shortcut_target(&shortcut)?,
        &target
    )?);
    assert_eq!(
        first_existing_projection_candidate(&tmp.path().join("Launcher").join("MyApp"))?,
        Some(shortcut)
    );
    Ok(())
}

#[test]
fn copy_recursively_preserves_executable_mode() -> Result<()> {
    let tmp = tempdir()?;
    let source = tmp.path().join("source.bin");
    let destination = tmp.path().join("nested/destination.bin");
    fs::write(&source, b"hello")?;
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(&source)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&source, permissions)?;
    }

    copy_recursively(&source, &destination)?;

    #[cfg(unix)]
    {
        assert_eq!(
            fs::metadata(&destination)?.permissions().mode() & 0o777,
            0o755
        );
    }
    Ok(())
}

#[test]
fn ensure_tree_writable_clears_readonly_on_files() -> Result<()> {
    let tmp = tempdir()?;
    let app_dir = tmp.path().join("MyApp.app");
    let binary = app_dir.join("Contents/MacOS/MyApp");
    fs::create_dir_all(binary.parent().expect("binary parent"))?;
    fs::write(&binary, b"unsigned-app")?;

    let mut permissions = fs::metadata(&binary)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&binary, permissions)?;

    ensure_tree_writable(&app_dir)?;

    assert!(!fs::metadata(&binary)?.permissions().readonly());
    Ok(())
}

#[test]
#[serial_test::serial]
fn materialize_fetch_cache_extracts_payload_tree() -> Result<()> {
    let tmp_home = tempdir()?;
    std::env::set_var("HOME", tmp_home.path());

    let payload_tar = {
        let mut out = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut out);
            append_tar_entry(
                &mut builder,
                DELIVERY_CONFIG_FILE,
                sample_delivery_toml().as_bytes(),
                0o644,
            )?;
            append_tar_entry(
                &mut builder,
                "MyApp.app/Contents/MacOS/MyApp",
                b"unsigned-app",
                0o644,
            )?;
            builder.finish()?;
        }
        out
    };
    let artifact = build_capsule_bytes(&payload_tar)?;
    let result =
        materialize_fetch_cache("local/my-app", "0.1.0", "http://127.0.0.1:8787", &artifact)?;

    assert!(result.cache_dir.exists());
    assert!(result.artifact_dir.join(DELIVERY_CONFIG_FILE).exists());
    assert!(result
        .artifact_dir
        .join("MyApp.app/Contents/MacOS/MyApp")
        .exists());
    let metadata = load_fetch_metadata(&result.cache_dir)?;
    assert_eq!(metadata.parent_digest, result.parent_digest);
    Ok(())
}

#[test]
#[serial_test::serial]
fn materialize_fetch_cache_preserves_executable_mode_from_payload() -> Result<()> {
    let tmp_home = tempdir()?;
    std::env::set_var("HOME", tmp_home.path());

    let payload_tar = {
        let mut out = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut out);
            append_tar_entry(
                &mut builder,
                DELIVERY_CONFIG_FILE,
                sample_delivery_toml().as_bytes(),
                0o644,
            )?;
            append_tar_entry(
                &mut builder,
                "MyApp.app/Contents/MacOS/MyApp",
                b"unsigned-app",
                0o755,
            )?;
            builder.finish()?;
        }
        out
    };
    let artifact = build_capsule_bytes(&payload_tar)?;
    let result =
        materialize_fetch_cache("local/my-app", "0.1.0", "http://127.0.0.1:8787", &artifact)?;

    #[cfg(unix)]
    {
        let binary = result.artifact_dir.join("MyApp.app/Contents/MacOS/MyApp");
        assert_ne!(fs::metadata(binary)?.permissions().mode() & 0o111, 0);
    }
    Ok(())
}

#[test]
fn project_creates_projection_metadata_without_mutating_derived_artifact() -> Result<()> {
    let tmp = tempdir()?;
    let metadata_root = tmp.path().join("projection-metadata");
    let launcher_dir = sample_projection_launcher_dir(tmp.path());
    let command_dir = sample_projection_command_dir(tmp.path());
    let (_derived_dir, derived_app) = sample_finalized_app(tmp.path())?;
    let digest_before = compute_tree_digest(&derived_app)?;

    let result = project_with_roots_and_command_dir(
        &derived_app,
        &launcher_dir,
        &metadata_root,
        &command_dir,
    )?;

    assert!(result.created);
    assert_eq!(result.state, "ok");
    assert_eq!(compute_tree_digest(&derived_app)?, digest_before);
    assert!(result.projected_path.exists());
    if cfg!(target_os = "linux") {
        let projected_meta = fs::symlink_metadata(&result.projected_path)?;
        assert!(projected_meta.is_file());
        let desktop = fs::read_to_string(&result.projected_path)?;
        assert!(desktop.contains("[Desktop Entry]"));
        assert!(desktop.contains("Exec="));
        let command_path = command_dir.join("my-app");
        assert!(fs::symlink_metadata(&command_path)?
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read_link(&command_path)?,
            sample_projection_binary_path(&derived_app)
        );
    } else if !cfg!(windows) {
        let symlink_meta = fs::symlink_metadata(&result.projected_path)?;
        assert!(symlink_meta.file_type().is_symlink());
    }
    assert!(result.metadata_path.exists());
    Ok(())
}

#[test]
fn project_rejects_name_conflict() -> Result<()> {
    if !host_supports_projection() {
        return Ok(());
    }
    let tmp = tempdir()?;
    let metadata_root = tmp.path().join("projection-metadata");
    let launcher_dir = sample_projection_launcher_dir(tmp.path());
    let command_dir = sample_projection_command_dir(tmp.path());
    let (_derived_dir, derived_app) = sample_finalized_app(tmp.path())?;
    fs::create_dir_all(&launcher_dir)?;
    let conflict_path = if cfg!(target_os = "linux") {
        launcher_dir.join("my-app.desktop")
    } else {
        launcher_dir.join("MyApp.app")
    };
    fs::write(conflict_path, b"occupied")?;

    let err = project_with_roots_and_command_dir(
        &derived_app,
        &launcher_dir,
        &metadata_root,
        &command_dir,
    )
    .expect_err("projection must reject name conflicts");
    assert!(err.to_string().contains("Projection name conflict"));
    Ok(())
}

#[test]
fn project_list_reports_broken_projection_when_target_missing() -> Result<()> {
    if !host_supports_projection() {
        return Ok(());
    }
    let tmp = tempdir()?;
    let metadata_root = tmp.path().join("projection-metadata");
    let launcher_dir = sample_projection_launcher_dir(tmp.path());
    let command_dir = sample_projection_command_dir(tmp.path());
    let (_derived_dir, derived_app) = sample_finalized_app(tmp.path())?;
    let result = project_with_roots_and_command_dir(
        &derived_app,
        &launcher_dir,
        &metadata_root,
        &command_dir,
    )?;
    let orphaned_app = tmp.path().join(if cfg!(target_os = "linux") {
        "my-app-orphaned"
    } else {
        "MyApp-orphaned.app"
    });
    fs::rename(&derived_app, orphaned_app)?;

    let listing = list_projections(&metadata_root)?;
    assert_eq!(listing.total, 1);
    assert_eq!(listing.broken, 1);
    assert_eq!(listing.projections[0].projection_id, result.projection_id);
    assert!(listing.projections[0]
        .problems
        .iter()
        .any(|problem| problem == "derived_app_missing"));
    Ok(())
}

#[test]
fn linux_project_list_reports_missing_command_symlink() -> Result<()> {
    if !cfg!(target_os = "linux") {
        return Ok(());
    }
    let tmp = tempdir()?;
    let metadata_root = tmp.path().join("projection-metadata");
    let launcher_dir = sample_projection_launcher_dir(tmp.path());
    let command_dir = sample_projection_command_dir(tmp.path());
    let (_derived_dir, derived_app) = sample_finalized_app_with_target(tmp.path(), "linux/x86_64")?;
    let result = project_with_roots_and_command_dir(
        &derived_app,
        &launcher_dir,
        &metadata_root,
        &command_dir,
    )?;
    fs::remove_file(command_dir.join("my-app"))?;

    let listing = list_projections(&metadata_root)?;
    assert_eq!(listing.total, 1);
    assert_eq!(listing.broken, 1);
    assert_eq!(listing.projections[0].projection_id, result.projection_id);
    assert!(listing.projections[0]
        .problems
        .iter()
        .any(|problem| problem == "projected_command_missing"));
    Ok(())
}

#[test]
fn unproject_removes_symlink_and_metadata_even_when_target_missing() -> Result<()> {
    if !host_supports_projection() {
        return Ok(());
    }
    let tmp = tempdir()?;
    let metadata_root = tmp.path().join("projection-metadata");
    let launcher_dir = sample_projection_launcher_dir(tmp.path());
    let command_dir = sample_projection_command_dir(tmp.path());
    let (_derived_dir, derived_app) = sample_finalized_app(tmp.path())?;
    let result = project_with_roots_and_command_dir(
        &derived_app,
        &launcher_dir,
        &metadata_root,
        &command_dir,
    )?;
    let orphaned_app = tmp.path().join(if cfg!(target_os = "linux") {
        "my-app-orphaned"
    } else {
        "MyApp-orphaned.app"
    });
    fs::rename(&derived_app, orphaned_app)?;

    let unprojected = unproject_with_metadata_root(&result.projection_id, &metadata_root)?;
    assert!(unprojected.removed_projected_path);
    assert!(unprojected.removed_metadata);
    assert!(!result.projected_path.exists());
    if cfg!(target_os = "linux") {
        assert!(!command_dir.join("my-app").exists());
    }
    assert!(!result.metadata_path.exists());
    Ok(())
}

#[test]
fn project_rejects_digest_mismatch() -> Result<()> {
    if !host_supports_projection() {
        return Ok(());
    }
    let tmp = tempdir()?;
    let metadata_root = tmp.path().join("projection-metadata");
    let launcher_dir = sample_projection_launcher_dir(tmp.path());
    let command_dir = sample_projection_command_dir(tmp.path());
    let (derived_dir, derived_app) = sample_finalized_app(tmp.path())?;
    fs::write(sample_projection_binary_path(&derived_app), b"tampered-app")?;

    let err = project_with_roots_and_command_dir(
        &derived_app,
        &launcher_dir,
        &metadata_root,
        &command_dir,
    )
    .expect_err("projection must reject digest mismatches");
    assert!(err.to_string().contains("Derived artifact digest mismatch"));
    assert!(derived_dir.join(PROVENANCE_FILE).exists());
    Ok(())
}

#[test]
fn project_rejects_mismatched_host_targets_even_with_valid_shape() -> Result<()> {
    let tmp = tempdir()?;
    let metadata_root = tmp.path().join("projection-metadata");
    let launcher_dir = sample_projection_launcher_dir(tmp.path());
    let command_dir = sample_projection_command_dir(tmp.path());
    let unsupported_target = match host_projection_os_family() {
        Some("linux") => "windows/x86_64",
        Some("windows") => "linux/x86_64",
        _ => "linux/x86_64",
    };
    let (_derived_dir, derived_app) =
        sample_finalized_app_with_target(tmp.path(), unsupported_target)?;

    let err = project_with_roots_and_command_dir(
        &derived_app,
        &launcher_dir,
        &metadata_root,
        &command_dir,
    )
    .expect_err("projection must fail closed for unsupported targets");

    assert!(err.to_string().contains("unsupported on this host"));
    Ok(())
}

fn append_tar_entry(
    builder: &mut tar::Builder<&mut Vec<u8>>,
    path: &str,
    bytes: &[u8],
    mode: u32,
) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(mode);
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    header.set_cksum();
    builder.append_data(&mut header, path, Cursor::new(bytes))?;
    Ok(())
}

fn build_capsule_bytes(payload_tar: &[u8]) -> Result<Vec<u8>> {
    let payload_tar_zst = zstd::stream::encode_all(Cursor::new(payload_tar), 3)?;
    let mut out = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut out);
        append_tar_entry(&mut builder, "capsule.toml", b"schema_version = \"0.2\"\nname = \"demo\"\nversion = \"0.1.0\"\ntype = \"app\"\ndefault_target = \"cli\"\n[targets.cli]\nruntime = \"static\"\npath = \"MyApp.app\"\n", 0o644)?;
        append_tar_entry(&mut builder, "payload.tar.zst", &payload_tar_zst, 0o644)?;
        builder.finish()?;
    }
    Ok(out)
}
