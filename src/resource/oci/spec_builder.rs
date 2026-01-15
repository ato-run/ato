use crate::capsule_types::capsule_v1::CapsuleManifestV1;
use crate::capsule_types::capsule_v1::{CapsuleExecution, RuntimeType, StorageVolume};
use crate::verification::path::validate_path;
use crate::workload::manifest_loader::ResourceRequirements;
use oci_spec::runtime::{
    HookBuilder, HooksBuilder, Linux, LinuxBuilder, LinuxNamespaceBuilder, LinuxNamespaceType,
    Mount, MountBuilder, ProcessBuilder, RootBuilder, Spec, SpecBuilder,
};
use std::path::{Path, PathBuf};

/// Validate volume mounts for security
fn validate_mounts(volumes: &[StorageVolume], allowed_paths: &[String]) -> Result<(), String> {
    for vol in volumes {
        // Validation logic for bind mounts
        if vol.name.starts_with("bind:") {
            let path_str = vol.name.strip_prefix("bind:").unwrap();
            validate_path(path_str, allowed_paths)
                .map_err(|e| format!("Volume source error: {}", e))?;
        }
    }
    Ok(())
}

fn derive_args(execution: &CapsuleExecution, extra_args: Option<&[String]>) -> Vec<String> {
    // UARC V1.1.0: Native is deprecated, treated same as Source
    #[allow(deprecated)]
    let is_native_or_source =
        matches!(execution.runtime, RuntimeType::Native | RuntimeType::Source);
    if is_native_or_source {
        let parts = shell_words::split(&execution.entrypoint)
            .unwrap_or_else(|_| vec![execution.entrypoint.clone()]);
        if parts.is_empty() {
            return vec![];
        }

        if let Some(extra) = extra_args {
            if !extra.is_empty() {
                let mut new_args = vec![parts[0].clone()];
                new_args.extend_from_slice(extra);
                new_args
            } else {
                parts
            }
        } else {
            parts
        }
    } else if let Some(extra) = extra_args {
        // For Docker/PythonUv runtimes, do not force a default command.
        // Leaving args empty preserves the image/runtime default entrypoint/CMD.
        extra.to_vec()
    } else {
        vec![]
    }
}

#[allow(clippy::too_many_arguments)]
pub fn build_oci_spec(
    rootfs_path: &Path,
    execution: &CapsuleExecution,
    volumes: &[StorageVolume],
    gpu_uuids: Option<&[String]>,
    allowed_host_paths: &[String],
    resources: Option<&ResourceRequirements>,
    extra_args: Option<&[String]>,
    _manifest: &CapsuleManifestV1,
) -> Result<Spec, String> {
    // --- 1. Build Process Configuration ---
    let mut process_envs: Vec<String> = execution
        .env
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect();

    if let Some(uuids) = gpu_uuids {
        if !uuids.is_empty() {
            let visible_devices = uuids.join(",");
            process_envs.push(format!("NVIDIA_VISIBLE_DEVICES={}", visible_devices));
            process_envs.push("NVIDIA_DRIVER_CAPABILITIES=compute,utility".to_string());
        }
    }

    // Determine Args
    let args = derive_args(execution, extra_args);

    let process = ProcessBuilder::default()
        .args(args)
        .env(process_envs)
        .cwd(PathBuf::from("/"))
        .no_new_privileges(true)
        .build()
        .map_err(|e| format!("Failed to build process config: {}", e))?;

    // --- 2. Build Hooks ---
    let hooks = if gpu_uuids.is_some() && !gpu_uuids.unwrap().is_empty() {
        let nvidia_hook = HookBuilder::default()
            .path(PathBuf::from("/usr/bin/nvidia-container-runtime-hook"))
            .args(vec![
                "nvidia-container-runtime-hook".to_string(),
                "prestart".to_string(),
            ])
            .build()
            .map_err(|e| format!("Failed to build NVIDIA hook: {}", e))?;

        Some(
            HooksBuilder::default()
                .prestart(vec![nvidia_hook])
                .build()
                .map_err(|e| format!("Failed to build hooks: {}", e))?,
        )
    } else {
        None
    };

    // Egress policy hooks were moved to capsule-cli; runtime enforces via eBPF/guard.

    // --- 3. Build Mounts ---
    let mut mounts = build_default_mounts();
    validate_mounts(volumes, allowed_host_paths)?;

    for vol in volumes {
        let (source_path, mount_type) = if vol.name.starts_with("bind:") {
            (vol.name.strip_prefix("bind:").unwrap().to_string(), "bind")
        } else {
            continue;
        };

        if mount_type == "bind" {
            let mut mount_options = vec!["rbind".to_string()];
            if vol.read_only {
                mount_options.push("ro".to_string());
            }

            let mount = MountBuilder::default()
                .source(PathBuf::from(&source_path))
                .destination(PathBuf::from(&vol.mount_path))
                .typ("bind".to_string())
                .options(mount_options)
                .build()
                .map_err(|e| format!("Failed to build mount for {}: {}", vol.mount_path, e))?;

            mounts.push(mount);
        }
    }

    // --- 4. Build Root Filesystem ---
    let root = RootBuilder::default()
        .path(rootfs_path.to_path_buf())
        .readonly(false)
        .build()
        .map_err(|e| format!("Failed to build root config: {}", e))?;

    // --- 5. Build Linux-specific Configuration ---
    let mut linux = build_default_linux();

    if let Some(res) = resources {
        if let Some(memory_bytes) = res.memory_bytes {
            use oci_spec::runtime::LinuxResources;
            use serde_json::json;
            let lr_value = json!({ "memory": { "limit": memory_bytes as i64 } });
            let lr: LinuxResources = serde_json::from_value(lr_value)
                .map_err(|e| format!("Failed to build LinuxResources: {}", e))?;
            linux = LinuxBuilder::default()
                .resources(lr)
                .build()
                .map_err(|e| format!("Failed to set Linux resources: {}", e))?;
        }
    }

    // --- 6. Assemble Final Spec ---
    let mut spec_builder = SpecBuilder::default()
        .version("1.0.2".to_string())
        .root(root)
        .process(process)
        .mounts(mounts)
        .linux(linux);

    if let Some(hooks_config) = hooks {
        spec_builder = spec_builder.hooks(hooks_config);
    }

    let spec = spec_builder
        .build()
        .map_err(|e| format!("Failed to build OCI spec: {}", e))?;

    Ok(spec)
}

fn build_default_mounts() -> Vec<Mount> {
    vec![
        MountBuilder::default()
            .destination(PathBuf::from("/proc"))
            .typ("proc".to_string())
            .source(PathBuf::from("proc"))
            .options(vec![
                "nosuid".to_string(),
                "noexec".to_string(),
                "nodev".to_string(),
            ])
            .build()
            .unwrap(),
        MountBuilder::default()
            .destination(PathBuf::from("/dev"))
            .typ("tmpfs".to_string())
            .source(PathBuf::from("tmpfs"))
            .options(vec![
                "nosuid".to_string(),
                "strictatime".to_string(),
                "mode=755".to_string(),
                "size=65536k".to_string(),
            ])
            .build()
            .unwrap(),
        MountBuilder::default()
            .destination(PathBuf::from("/dev/pts"))
            .typ("devpts".to_string())
            .source(PathBuf::from("devpts"))
            .options(vec![
                "nosuid".to_string(),
                "noexec".to_string(),
                "newinstance".to_string(),
                "ptmxmode=0666".to_string(),
                "mode=0620".to_string(),
            ])
            .build()
            .unwrap(),
        MountBuilder::default()
            .destination(PathBuf::from("/sys"))
            .typ("sysfs".to_string())
            .source(PathBuf::from("sysfs"))
            .options(vec![
                "nosuid".to_string(),
                "noexec".to_string(),
                "nodev".to_string(),
                "ro".to_string(),
            ])
            .build()
            .unwrap(),
    ]
}

fn build_default_linux() -> Linux {
    let namespaces = vec![
        LinuxNamespaceBuilder::default()
            .typ(LinuxNamespaceType::Pid)
            .build()
            .unwrap(),
        LinuxNamespaceBuilder::default()
            .typ(LinuxNamespaceType::Network)
            .build()
            .unwrap(),
        LinuxNamespaceBuilder::default()
            .typ(LinuxNamespaceType::Ipc)
            .build()
            .unwrap(),
        LinuxNamespaceBuilder::default()
            .typ(LinuxNamespaceType::Uts)
            .build()
            .unwrap(),
        LinuxNamespaceBuilder::default()
            .typ(LinuxNamespaceType::Mount)
            .build()
            .unwrap(),
    ];
    LinuxBuilder::default()
        .namespaces(namespaces)
        .build()
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capsule_types::capsule_v1::{CapsuleMetadataV1, CapsuleRouting};
    use std::collections::HashMap;

    fn bind_volume(source: &str) -> StorageVolume {
        StorageVolume {
            name: format!("bind:{source}"),
            mount_path: "/mnt".to_string(),
            read_only: true,
            size_bytes: 0,
            use_thin: None,
            encrypted: false,
        }
    }

    fn test_manifest() -> CapsuleManifestV1 {
        CapsuleManifestV1 {
            schema_version: "1.0".to_string(),
            name: "test".to_string(),
            version: "0.0.1".to_string(),
            capsule_type: crate::capsule_types::capsule_v1::CapsuleType::App,
            metadata: CapsuleMetadataV1::default(),
            capabilities: None,
            requirements: crate::capsule_types::capsule_v1::CapsuleRequirements::default(),
            execution: CapsuleExecution {
                runtime: RuntimeType::Source, // UARC V1.1.0: Use Source instead of deprecated Native
                entrypoint: "/bin/echo".to_string(),
                port: None,
                health_check: None,
                startup_timeout: 60,
                env: HashMap::new(),
                signals: Default::default(),
            },
            storage: crate::capsule_types::capsule_v1::CapsuleStorage::default(),
            routing: CapsuleRouting::default(),
            network: None,
            model: None,
            transparency: None,
            pool: None,
            build: None,
            isolation: None,
            targets: None,
            services: None,
        }
    }

    #[test]
    fn validate_mounts_allows_bind_mount_when_in_allowlist() {
        let allowed = vec!["/opt/models".to_string()];
        let vols = vec![bind_volume("/opt/models/llama.gguf")];
        assert!(super::validate_mounts(&vols, &allowed).is_ok());
    }

    #[test]
    fn validate_mounts_denies_bind_mount_when_not_in_allowlist() {
        let allowed = vec!["/opt/models".to_string()];
        let vols = vec![bind_volume("/etc/shadow")];
        let err = super::validate_mounts(&vols, &allowed).unwrap_err();
        assert!(err.contains("not in the allowed paths"));
    }

    #[test]
    fn validate_mounts_denies_traversal_components() {
        let allowed = vec!["/opt/models".to_string()];
        let vols = vec![bind_volume("/opt/models/../etc/passwd")];
        let err = super::validate_mounts(&vols, &allowed).unwrap_err();
        assert!(err.contains("Path traversal detected"));
    }

    #[test]
    fn validate_mounts_ok_when_no_volumes() {
        let allowed: Vec<String> = vec![];
        assert!(super::validate_mounts(&[], &allowed).is_ok());
    }

    #[test]
    fn build_oci_spec_sets_ro_option_for_readonly_mount() {
        let temp = tempfile::tempdir().expect("tempdir");
        let rootfs = temp.path();

        let exec = CapsuleExecution {
            runtime: RuntimeType::Source, // UARC V1.1.0: Use Source instead of deprecated Native
            entrypoint: "/bin/echo".to_string(),
            port: None,
            health_check: None,
            startup_timeout: 60,
            env: HashMap::new(),
            signals: Default::default(),
        };
        let vols = vec![StorageVolume {
            name: "bind:/tmp/gumball/cache".to_string(),
            mount_path: "/data/models".to_string(),
            read_only: true,
            size_bytes: 0,
            use_thin: None,
            encrypted: false,
        }];
        let allowed = vec!["/tmp".to_string()];
        let manifest = test_manifest();

        let spec = build_oci_spec(rootfs, &exec, &vols, None, &allowed, None, None, &manifest)
            .expect("build_oci_spec should succeed");

        let mounts = spec.mounts().as_ref().expect("mounts should exist");
        let data_mount = mounts
            .iter()
            .find(|m| m.destination().as_path() == std::path::Path::new("/data/models"))
            .expect("expected /data/models mount");

        let opts = data_mount
            .options()
            .as_ref()
            .expect("mount options should exist");
        assert!(opts.iter().any(|o| o == "ro"));
    }
}
