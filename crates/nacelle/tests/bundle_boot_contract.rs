#[cfg(unix)]
mod unix_tests {
    use std::fs;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use nacelle::common::constants::BUNDLE_MAGIC;

    fn nacelle_bin() -> &'static str {
        env!("CARGO_BIN_EXE_nacelle")
    }

    fn copy_executable(src: &Path, dest: &Path) {
        fs::copy(src, dest).unwrap();
        let mut perms = fs::metadata(dest).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        fs::set_permissions(dest, perms).unwrap();
    }

    fn make_bundle_image(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            for (path, body) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_path(path).unwrap();
                header.set_size(body.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder.append(&header, *body).unwrap();
            }
            builder.finish().unwrap();
        }

        let compressed = zstd::encode_all(tar_bytes.as_slice(), 0).unwrap();
        let mut image = Vec::new();
        image.extend_from_slice(&compressed);
        image.extend_from_slice(BUNDLE_MAGIC);
        image.extend_from_slice(&(compressed.len() as u64).to_le_bytes());
        image
    }

    fn write_bundled_binary(dest: &Path, entries: &[(&str, &[u8])]) -> PathBuf {
        copy_executable(Path::new(nacelle_bin()), dest);
        let mut file = fs::OpenOptions::new().append(true).open(dest).unwrap();
        file.write_all(&make_bundle_image(entries)).unwrap();
        dest.to_path_buf()
    }

    #[test]
    fn bundled_runtime_boots_valid_config() {
        let temp_dir = tempfile::tempdir().unwrap();
        let bundled_path = temp_dir.path().join("nacelle-bundled");
        let config = br#"{
  "version": "1.0.0",
  "services": {
    "main": {
      "executable": "/bin/sh",
      "args": ["-c", "exit 0"],
      "cwd": "."
    }
  },
  "sandbox": {
    "enabled": false,
    "network": {
      "enabled": false,
      "enforcement": "best_effort"
    }
  }
}"#;

        write_bundled_binary(&bundled_path, &[("config.json", config)]);

        let output = Command::new(&bundled_path).output().unwrap();
        assert!(
            output.status.success(),
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn bundled_runtime_fails_without_config_json() {
        let temp_dir = tempfile::tempdir().unwrap();
        let bundled_path = temp_dir.path().join("nacelle-missing-config");
        write_bundled_binary(&bundled_path, &[("README.txt", b"missing config")]);

        let output = Command::new(&bundled_path).output().unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("No config.json found in bundle"),
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn bundled_runtime_fails_with_invalid_config_json() {
        let temp_dir = tempfile::tempdir().unwrap();
        let bundled_path = temp_dir.path().join("nacelle-invalid-config");
        write_bundled_binary(&bundled_path, &[("config.json", b"{not-json}")]);

        let output = Command::new(&bundled_path).output().unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("Failed to parse config.json"),
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn bundled_runtime_can_preserve_extracted_dir_for_debugging() {
        let temp_dir = tempfile::tempdir().unwrap();
        let bundled_path = temp_dir.path().join("nacelle-keep-extracted");
        let config = br#"{
  "version": "1.0.0",
  "services": {
    "main": {
      "executable": "/bin/sh",
      "args": ["-c", "exit 0"],
      "cwd": "."
    }
  },
  "sandbox": {
    "enabled": false,
    "network": {
      "enabled": false,
      "enforcement": "best_effort"
    }
  }
}"#;
        write_bundled_binary(&bundled_path, &[("config.json", config)]);

        let output = Command::new(&bundled_path)
            .env("NACELLE_BUNDLE_KEEP_EXTRACTED", "1")
            .output()
            .unwrap();
        assert!(output.status.success());

        let stderr = String::from_utf8_lossy(&output.stderr);
        let preserved_path = stderr
            .lines()
            .find_map(|line| line.strip_prefix("Preserving extracted bundle contents at "))
            .map(PathBuf::from)
            .unwrap_or_else(|| panic!("stderr: {}", stderr));

        assert!(preserved_path.join("config.json").exists());
        fs::remove_dir_all(preserved_path).unwrap();
    }
}
