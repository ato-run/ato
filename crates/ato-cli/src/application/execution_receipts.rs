use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use capsule_core::common::paths::ato_executions_dir;
use capsule_core::execution_identity::ExecutionReceipt;

const RECEIPT_FILE_NAME: &str = "receipt.json";

pub(crate) fn default_receipt_root() -> PathBuf {
    ato_executions_dir()
}

pub(crate) fn receipt_dir(root: &Path, execution_id: &str) -> Result<PathBuf> {
    Ok(root.join(execution_dir_name(execution_id)?))
}

pub(crate) fn receipt_path(root: &Path, execution_id: &str) -> Result<PathBuf> {
    Ok(receipt_dir(root, execution_id)?.join(RECEIPT_FILE_NAME))
}

pub(crate) fn write_receipt_atomic(receipt: &ExecutionReceipt) -> Result<PathBuf> {
    write_receipt_atomic_at(&default_receipt_root(), receipt)
}

pub(crate) fn write_receipt_atomic_at(root: &Path, receipt: &ExecutionReceipt) -> Result<PathBuf> {
    let dir = receipt_dir(root, &receipt.execution_id)?;
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create execution receipt dir {}", dir.display()))?;
    let final_path = dir.join(RECEIPT_FILE_NAME);
    let tmp_path = dir.join(format!(".receipt.json.tmp.{}", std::process::id()));
    let payload = serde_json::to_vec_pretty(receipt).with_context(|| {
        format!(
            "failed to encode execution receipt {}",
            receipt.execution_id
        )
    })?;

    {
        let mut tmp = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp_path)
            .with_context(|| format!("failed to open temp receipt {}", tmp_path.display()))?;
        tmp.write_all(&payload)
            .with_context(|| format!("failed to write temp receipt {}", tmp_path.display()))?;
        let _ = tmp.sync_all();
    }

    if let Err(err) = fs::rename(&tmp_path, &final_path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(err).with_context(|| {
            format!(
                "failed to rename {} -> {}",
                tmp_path.display(),
                final_path.display()
            )
        });
    }

    Ok(final_path)
}

pub(crate) fn read_receipt(execution_id: &str) -> Result<ExecutionReceipt> {
    read_receipt_at(&default_receipt_root(), execution_id)
}

pub(crate) fn read_receipt_at(root: &Path, execution_id: &str) -> Result<ExecutionReceipt> {
    let path = receipt_path(root, execution_id)?;
    let raw = fs::read(&path)
        .with_context(|| format!("failed to read execution receipt {}", path.display()))?;
    serde_json::from_slice(&raw)
        .with_context(|| format!("failed to parse execution receipt {}", path.display()))
}

fn execution_dir_name(execution_id: &str) -> Result<String> {
    let trimmed = execution_id.trim();
    anyhow::ensure!(!trimmed.is_empty(), "execution_id must not be empty");
    anyhow::ensure!(
        !trimmed.contains('/') && !trimmed.contains('\\'),
        "execution_id must not contain path separators"
    );
    anyhow::ensure!(
        trimmed != "." && trimmed != "..",
        "execution_id must not be a relative path component"
    );
    Ok(trimmed.replace(':', "_"))
}

#[cfg(test)]
mod tests {
    use capsule_core::execution_identity::{
        DependencyIdentity, EnvironmentIdentity, EnvironmentMode, ExecutionIdentityInput,
        ExecutionReceipt, FilesystemIdentity, LaunchIdentity, PlatformIdentity, PolicyIdentity,
        ReproducibilityCause, ReproducibilityClass, ReproducibilityIdentity, RuntimeIdentity,
        SourceIdentity, Tracked,
    };
    use tempfile::tempdir;

    use super::*;

    fn sample_receipt() -> ExecutionReceipt {
        let input = ExecutionIdentityInput::new(
            SourceIdentity {
                source_ref: Tracked::known("local:/app".to_string()),
                source_tree_hash: Tracked::known("blake3:source".to_string()),
            },
            DependencyIdentity {
                derivation_hash: Tracked::unknown("not observed"),
                output_hash: Tracked::unknown("not observed"),
            },
            RuntimeIdentity {
                declared: Some("node@20".to_string()),
                resolved: Some("node@20.10.0".to_string()),
                binary_hash: Tracked::unknown("not observed"),
                dynamic_linkage: Tracked::untracked("not implemented"),
                platform: PlatformIdentity {
                    os: "macos".to_string(),
                    arch: "aarch64".to_string(),
                    libc: "unknown".to_string(),
                },
            },
            EnvironmentIdentity {
                closure_hash: Tracked::known("blake3:env".to_string()),
                mode: EnvironmentMode::Closed,
                tracked_keys: vec!["PATH".to_string()],
                redacted_keys: Vec::new(),
                unknown_keys: Vec::new(),
            },
            FilesystemIdentity {
                view_hash: Tracked::known("blake3:fs".to_string()),
                projection_strategy: "direct".to_string(),
                writable_dirs: Vec::new(),
                persistent_state: Vec::new(),
                known_readonly_layers: Vec::new(),
            },
            PolicyIdentity {
                network_policy_hash: Tracked::known("blake3:network".to_string()),
                capability_policy_hash: Tracked::known("blake3:capability".to_string()),
                sandbox_policy_hash: Tracked::known("blake3:sandbox".to_string()),
            },
            LaunchIdentity {
                entry_point: "node".to_string(),
                argv: vec!["server.js".to_string()],
                working_directory: "/app".to_string(),
            },
            ReproducibilityIdentity {
                class: ReproducibilityClass::BestEffort,
                causes: vec![ReproducibilityCause::UnknownDependencyOutput],
            },
        );
        ExecutionReceipt::from_input(input, "2026-05-03T00:00:00Z".to_string()).expect("receipt")
    }

    #[test]
    fn write_then_read_receipt_round_trips() {
        let temp = tempdir().expect("tempdir");
        let receipt = sample_receipt();
        let path = write_receipt_atomic_at(temp.path(), &receipt).expect("write receipt");

        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("receipt.json")
        );

        let read = read_receipt_at(temp.path(), &receipt.execution_id).expect("read receipt");
        assert_eq!(read.execution_id, receipt.execution_id);
        assert_eq!(
            read.dependencies.output_hash.reason,
            receipt.dependencies.output_hash.reason
        );
    }

    #[test]
    fn receipt_path_sanitizes_algorithm_separator_for_portability() {
        let temp = tempdir().expect("tempdir");
        let path = receipt_path(temp.path(), "blake3:abc123").expect("path");
        assert!(path.ends_with(Path::new("blake3_abc123").join("receipt.json")));
    }

    #[test]
    fn receipt_path_rejects_path_separators() {
        let temp = tempdir().expect("tempdir");
        let err = receipt_path(temp.path(), "../blake3:abc123").unwrap_err();
        assert!(err.to_string().contains("path separators"));
    }

    #[test]
    fn atomic_write_does_not_leave_temp_file_on_success() {
        let temp = tempdir().expect("tempdir");
        let receipt = sample_receipt();
        let dir = receipt_dir(temp.path(), &receipt.execution_id).expect("dir");

        write_receipt_atomic_at(temp.path(), &receipt).expect("write receipt");

        let entries = fs::read_dir(dir)
            .expect("read dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(entries, vec!["receipt.json".to_string()]);
    }
}
