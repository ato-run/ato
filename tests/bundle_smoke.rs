use std::fs;

use nacelle::bundle;

#[test]
fn test_extract_and_run_embedded_python_from_bundle() {
    let temp = tempfile::tempdir().expect("tempdir");
    let work_dir = temp.path().join("work");
    let runtime_root = work_dir.join("runtime_src");
    let source_root = work_dir.join("source_src");
    fs::create_dir_all(runtime_root.join("python/bin")).unwrap();
    fs::create_dir_all(&source_root).unwrap();

    // Create a fake embedded python binary (shell script) so we can verify
    // the bundle runs WITHOUT relying on host python.
    let python_bin = runtime_root.join("python/bin/python3");
    fs::write(
        &python_bin,
        "#!/bin/sh\necho EMBEDDED_PYTHON\nexit 0\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&python_bin).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&python_bin, perms).unwrap();
    }

    // Minimal capsule.toml that v2 bundle boot logic can read.
    fs::write(
        source_root.join("capsule.toml"),
        r#"schema_version = "1.1"
name = "bundle-smoke"
version = "0.1.0"

[targets]
preference = ["source"]

[targets.source]
language = "python"
entrypoint = "main.py"
"#,
    )
    .unwrap();
    fs::write(source_root.join("main.py"), "print('hello')\n").unwrap();

    // Create tar archive in memory matching pack_v2 layout.
    let mut tar_builder = tar::Builder::new(Vec::new());
    tar_builder
        .append_dir_all("runtime", &runtime_root)
        .unwrap();
    tar_builder
        .append_dir_all("source", &source_root)
        .unwrap();
    tar_builder.finish().unwrap();
    let tar_bytes = tar_builder.into_inner().unwrap();

    // Compress with zstd and append magic + size.
    let compressed = zstd::encode_all(tar_bytes.as_slice(), 19).unwrap();

    let mut fake_exe: Vec<u8> = Vec::new();
    fake_exe.extend_from_slice(b"FAKE_EXE\0\0\0");
    fake_exe.extend_from_slice(&compressed);
    fake_exe.extend_from_slice(bundle::BUNDLE_MAGIC);
    fake_exe.extend_from_slice(&(compressed.len() as u64).to_le_bytes());

    let bundle_path = temp.path().join("bundle.bin");
    fs::write(&bundle_path, &fake_exe).unwrap();

    // Extract bundle.
    let extract_dir = temp.path().join("extracted");
    fs::create_dir_all(&extract_dir).unwrap();
    bundle::extract_bundle_to_dir(&bundle_path, &extract_dir).unwrap();

    let extracted_source = extract_dir.join("source");
    let extracted_runtime = extract_dir.join("runtime");

    assert!(extracted_source.join("capsule.toml").exists());
    assert!(extracted_source.join("main.py").exists());
    assert!(extracted_runtime.join("python/bin/python3").exists());

    // Build command using embedded runtime and execute.
    let entrypoint = bundle::read_entrypoint_from_manifest(&extracted_source.join("capsule.toml"))
        .unwrap();

    let language = bundle::read_source_language_from_manifest(&extracted_source.join("capsule.toml"))
        .unwrap();
    let mut cmd = bundle::build_bundle_command(
        language.as_deref(),
        &entrypoint,
        &extracted_source,
        &extracted_runtime,
    )
    .unwrap();
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let output = cmd.output().unwrap();
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("EMBEDDED_PYTHON"));
}

#[test]
fn test_extract_and_run_embedded_node_from_bundle() {
    let temp = tempfile::tempdir().expect("tempdir");
    let work_dir = temp.path().join("work");
    let runtime_root = work_dir.join("runtime_src");
    let source_root = work_dir.join("source_src");
    fs::create_dir_all(runtime_root.join("node/bin")).unwrap();
    fs::create_dir_all(&source_root).unwrap();

    // Fake embedded node binary.
    let node_bin = runtime_root.join("node/bin/node");
    fs::write(&node_bin, "#!/bin/sh\necho EMBEDDED_NODE\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&node_bin).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&node_bin, perms).unwrap();
    }

    fs::write(
        source_root.join("capsule.toml"),
        r#"schema_version = "1.1"
name = "bundle-smoke-node"
version = "0.1.0"

[targets]
preference = ["source"]

[targets.source]
language = "node"
entrypoint = "main.js"
"#,
    )
    .unwrap();
    fs::write(source_root.join("main.js"), "console.log('hello')\n").unwrap();

    let mut tar_builder = tar::Builder::new(Vec::new());
    tar_builder.append_dir_all("runtime", &runtime_root).unwrap();
    tar_builder.append_dir_all("source", &source_root).unwrap();
    tar_builder.finish().unwrap();
    let tar_bytes = tar_builder.into_inner().unwrap();

    let compressed = zstd::encode_all(tar_bytes.as_slice(), 19).unwrap();

    let mut fake_exe: Vec<u8> = Vec::new();
    fake_exe.extend_from_slice(b"FAKE_EXE\0\0\0");
    fake_exe.extend_from_slice(&compressed);
    fake_exe.extend_from_slice(bundle::BUNDLE_MAGIC);
    fake_exe.extend_from_slice(&(compressed.len() as u64).to_le_bytes());

    let bundle_path = temp.path().join("bundle.bin");
    fs::write(&bundle_path, &fake_exe).unwrap();

    let extract_dir = temp.path().join("extracted");
    fs::create_dir_all(&extract_dir).unwrap();
    bundle::extract_bundle_to_dir(&bundle_path, &extract_dir).unwrap();

    let extracted_source = extract_dir.join("source");
    let extracted_runtime = extract_dir.join("runtime");

    let entrypoint = bundle::read_entrypoint_from_manifest(&extracted_source.join("capsule.toml"))
        .unwrap();
    let language = bundle::read_source_language_from_manifest(&extracted_source.join("capsule.toml"))
        .unwrap();

    let mut cmd = bundle::build_bundle_command(
        language.as_deref(),
        &entrypoint,
        &extracted_source,
        &extracted_runtime,
    )
    .unwrap();
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let output = cmd.output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("EMBEDDED_NODE"));
}
