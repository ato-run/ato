use crate::proto::onescluster::common::v1 as common;
use capsule_core::capsule_v1::{
    CapsuleExecution, CapsuleManifestV1, CapsuleRequirements, CapsuleRouting, CapsuleStorage,
    CapsuleType, RuntimeType, StorageVolume,
};
use std::collections::HashMap;

/// Result of converting a RunPlan proto into the canonical CapsuleManifestV1.
pub struct RunPlanConversion {
    pub adep: CapsuleManifestV1,
    pub oci_image: String, // Kept for convenience
    pub digest: String,
}

/// Convert coordinator RunPlan proto into a CapsuleManifestV1 with best-effort field mapping.
pub fn from_coordinator(plan: &common::RunPlan) -> RunPlanConversion {
    let name = if !plan.name.is_empty() {
        plan.name.clone()
    } else if !plan.capsule_id.is_empty() {
        plan.capsule_id.clone()
    } else {
        "capsule".to_string()
    };

    let mut requirements = CapsuleRequirements::default();
    if let Some(vram_gb) = parse_vram_gb_hint(&plan.gpu_profile) {
        requirements.vram_min = Some(format!("{}GB", vram_gb));
    }

    let mut env = HashMap::new();
    if let Some(runtime_env) = runtime_env(plan) {
        for (k, v) in runtime_env {
            env.insert(k, v);
        }
    }

    let mut oci_image = String::new();
    let mut digest = String::new();
    let mut execution = CapsuleExecution {
        runtime: RuntimeType::Native,
        entrypoint: "".to_string(),
        port: None,
        health_check: None,
        startup_timeout: 60,
        env: env.clone(),
        signals: Default::default(),
    };
    let mut storage = CapsuleStorage::default();

    match &plan.runtime {
        Some(common::run_plan::Runtime::Docker(docker)) => {
            oci_image = docker.image.clone();
            digest = docker.digest.clone();

            execution.runtime = RuntimeType::Docker;
            execution.entrypoint = docker.image.clone();
            execution.env = env.clone();

            if let Some(port) = first_port(&docker.ports) {
                execution.port = Some(port);
            }

            storage.volumes = docker
                .mounts
                .iter()
                .map(|m| StorageVolume {
                    name: format!("bind:{}", m.source),
                    mount_path: m.target.clone(),
                    read_only: m.readonly,
                    size_bytes: 0,
                    use_thin: None,
                    encrypted: false,
                })
                .collect();
        }
        Some(common::run_plan::Runtime::Native(native)) => {
            execution.runtime = RuntimeType::Native;
            execution.entrypoint = native.binary_path.clone();
            execution.env = env.clone();
        }
        Some(common::run_plan::Runtime::Wasm(wasm)) => {
            execution.runtime = RuntimeType::Wasm;
            execution.entrypoint = wasm.component.clone();
            execution.env = env.clone();
        }
        Some(common::run_plan::Runtime::Source(source)) => {
            // Generic Source Runtime for interpreted languages
            execution.runtime = RuntimeType::Source;
            execution.entrypoint = source.entrypoint.clone();
            // Store cmd in entrypoint for the SourceRuntime to parse
            if !source.cmd.is_empty() {
                execution.entrypoint = source.cmd.join(" ");
            }
            execution.env = env.clone();
        }
        None => {
            // Default
        }
    }

    let manifest = CapsuleManifestV1 {
        schema_version: "1.0".to_string(),
        name,
        version: plan.version.clone(),
        capsule_type: CapsuleType::App,
        metadata: Default::default(),
        capabilities: None,
        requirements,
        execution,
        storage,
        routing: CapsuleRouting::default(),
        network: None,
        model: None,
        transparency: None,
        pool: None,
        targets: None,
    };

    RunPlanConversion {
        adep: manifest,
        oci_image,
        digest,
    }
}

pub fn from_engine(plan: &common::RunPlan) -> RunPlanConversion {
    from_coordinator(plan)
}

fn runtime_env(plan: &common::RunPlan) -> Option<HashMap<String, String>> {
    match &plan.runtime {
        Some(common::run_plan::Runtime::Docker(docker)) => Some(
            docker
                .env
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        ),
        Some(common::run_plan::Runtime::Native(native)) => Some(
            native
                .env
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        ),
        Some(common::run_plan::Runtime::Wasm(wasm)) => Some(
            wasm.env
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        ),
        Some(common::run_plan::Runtime::Source(source)) => Some(
            source
                .env
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        ),
        None => None,
    }
}

fn first_port(ports: &[common::Port]) -> Option<u16> {
    ports.first().map(|p| {
        if p.host_port != 0 {
            p.host_port as u16
        } else {
            p.container_port as u16
        }
    })
}

fn parse_vram_gb_hint(profile: &str) -> Option<u64> {
    if let Some(stripped) = profile.strip_suffix("GB") {
        if let Ok(num) = stripped.trim().parse::<u64>() {
            return Some(num);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::onescluster::common::v1::{self as common, run_plan};

    #[test]
    fn test_docker_mapping() {
        let plan = common::RunPlan {
            capsule_id: "test-capsule".to_string(),
            name: "Test Capsule".to_string(),
            version: "1.0.0".to_string(),
            runtime: Some(run_plan::Runtime::Docker(common::DockerRuntime {
                image: "nginx:latest".to_string(),
                digest: "sha256:12345".to_string(),
                command: vec![
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    "echo hello".to_string(),
                ],
                env: HashMap::from([("KEY".to_string(), "VALUE".to_string())]),
                working_dir: "/app".to_string(),
                user: "1000".to_string(),
                ports: vec![common::Port {
                    container_port: 80,
                    host_port: 8080,
                    protocol: "tcp".to_string(),
                }],
                mounts: vec![common::Mount {
                    source: "/host/path".to_string(),
                    target: "/container/path".to_string(),
                    readonly: true,
                }],
            })),
            cpu_cores: 0,
            memory_bytes: 0,
            gpu_profile: "10GB".to_string(),
            egress_allowlist: vec![],
        };

        let conversion = from_coordinator(&plan);
        let manifest = conversion.adep;

        // Check top-level
        assert_eq!(manifest.name, "Test Capsule");
        assert_eq!(manifest.version, "1.0.0");

        // Check Execution
        assert_eq!(manifest.execution.runtime, RuntimeType::Docker);
        assert_eq!(manifest.execution.entrypoint, "nginx:latest");
        assert_eq!(manifest.execution.env.get("KEY").unwrap(), "VALUE");
        assert_eq!(manifest.execution.port, Some(8080));

        // Check Requirements (GPU parsing)
        assert_eq!(manifest.requirements.vram_min, Some("10GB".to_string()));

        // Check Storage (Bind mount mapping)
        assert_eq!(manifest.storage.volumes.len(), 1);
        let vol = &manifest.storage.volumes[0];
        assert_eq!(vol.name, "bind:/host/path");
        assert_eq!(vol.mount_path, "/container/path");
        assert!(vol.read_only);
    }

    #[test]
    fn test_native_mapping_args_check() {
        let plan = common::RunPlan {
            capsule_id: "native-capsule".to_string(),
            name: "Native Test".to_string(),
            version: "0.1.0".to_string(),
            runtime: Some(run_plan::Runtime::Native(common::NativeRuntime {
                binary_path: "/usr/bin/python3".to_string(),
                args: vec!["app.py".to_string(), "--flag".to_string()],
                env: HashMap::new(),
                working_dir: "".to_string(),
            })),
            cpu_cores: 0,
            memory_bytes: 0,
            gpu_profile: "".to_string(),
            egress_allowlist: vec![],
        };

        let conversion = from_coordinator(&plan);
        let manifest = conversion.adep;

        assert_eq!(manifest.execution.runtime, RuntimeType::Native);

        // This test intentionally documents the behavior: args are NOT in the manifest.
        assert_eq!(manifest.execution.entrypoint, "/usr/bin/python3");
    }
}
