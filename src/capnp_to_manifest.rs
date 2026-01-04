//! Cap'n Proto ↔ CapsuleManifestV1 conversion.
//!
//! This module provides canonical Cap'n Proto serialization for UARC V1.1.0.
//! The Cap'n Proto bytes are the sole signing ground truth per UARC spec.

use crate::capsule_capnp;
use capsule_core::capsule_v1::{
    CapsuleManifestV1, CapsuleType, EgressIdType, Platform, Quantization, RouteWeight, RuntimeType,
    TransparencyLevel,
};
use capsule_core::utils::parse_memory_string;

/// Error type for Cap'n Proto conversion
#[derive(Debug, thiserror::Error)]
pub enum CapnpConversionError {
    #[error("Cap'n Proto read error: {0}")]
    ReadError(String),
    #[error("Missing required field: {0}")]
    MissingField(&'static str),
    #[error("Invalid enum value for {field}: {value}")]
    InvalidEnum { field: &'static str, value: String },
}

impl From<capnp::Error> for CapnpConversionError {
    fn from(e: capnp::Error) -> Self {
        CapnpConversionError::ReadError(e.to_string())
    }
}

// ============================================================================
// Enum conversions: Rust -> Cap'n Proto
// ============================================================================

fn capsule_type_to_capnp(t: CapsuleType) -> capsule_capnp::CapsuleType {
    match t {
        CapsuleType::Inference => capsule_capnp::CapsuleType::Inference,
        CapsuleType::Tool => capsule_capnp::CapsuleType::Tool,
        CapsuleType::App => capsule_capnp::CapsuleType::App,
    }
}

fn runtime_type_to_capnp(r: RuntimeType) -> capsule_capnp::RuntimeType {
    match r {
        RuntimeType::PythonUv => capsule_capnp::RuntimeType::PythonUv,
        RuntimeType::Docker => capsule_capnp::RuntimeType::Docker,
        RuntimeType::Native => capsule_capnp::RuntimeType::Native,
        RuntimeType::Youki => capsule_capnp::RuntimeType::Youki,
        RuntimeType::Wasm => capsule_capnp::RuntimeType::Native, // Treat Wasm as Native for Cap'n Proto
    }
}

fn route_weight_to_capnp(w: RouteWeight) -> capsule_capnp::RouteWeight {
    match w {
        RouteWeight::Light => capsule_capnp::RouteWeight::Light,
        RouteWeight::Heavy => capsule_capnp::RouteWeight::Heavy,
    }
}

fn quantization_to_capnp(q: Quantization) -> capsule_capnp::Quantization {
    match q {
        Quantization::Fp16 => capsule_capnp::Quantization::Fp16,
        Quantization::Bf16 => capsule_capnp::Quantization::Bf16,
        Quantization::Bit8 => capsule_capnp::Quantization::Bit8,
        Quantization::Bit4 => capsule_capnp::Quantization::Bit4,
    }
}

fn egress_id_type_to_capnp(t: EgressIdType) -> capsule_capnp::EgressIdType {
    match t {
        EgressIdType::Ip => capsule_capnp::EgressIdType::Ip,
        EgressIdType::Cidr => capsule_capnp::EgressIdType::Cidr,
        EgressIdType::Spiffe => capsule_capnp::EgressIdType::Spiffe,
    }
}

fn transparency_level_to_capnp(t: TransparencyLevel) -> capsule_capnp::TransparencyLevel {
    match t {
        TransparencyLevel::Strict => capsule_capnp::TransparencyLevel::Strict,
        TransparencyLevel::Loose => capsule_capnp::TransparencyLevel::Loose,
        TransparencyLevel::Off => capsule_capnp::TransparencyLevel::Off,
    }
}

/// Serialize a CapsuleManifestV1 to canonical Cap'n Proto bytes.
///
/// This function produces deterministic output regardless of how the manifest
/// was originally constructed (from JSON, TOML, or Cap'n Proto). The output
/// is suitable for signature verification as per UARC V1.1.0 Normative Decision #2.
pub fn manifest_to_capnp_bytes(
    manifest: &CapsuleManifestV1,
) -> Result<Vec<u8>, CapnpConversionError> {
    use capsule_capnp::capsule_manifest;

    let mut message = capnp::message::Builder::new_default();

    {
        let mut builder = message.init_root::<capsule_manifest::Builder>();

        builder.set_schema_version(&manifest.schema_version);
        builder.set_name(&manifest.name);
        builder.set_version(&manifest.version);
        builder.set_type(capsule_type_to_capnp(manifest.capsule_type));

        // Metadata
        {
            let mut meta = builder.reborrow().init_metadata();
            if let Some(display_name) = &manifest.metadata.display_name {
                meta.set_display_name(display_name);
            }
            if let Some(description) = &manifest.metadata.description {
                meta.set_description(description);
            }
            if let Some(author) = &manifest.metadata.author {
                meta.set_author(author);
            }
            if let Some(icon) = &manifest.metadata.icon {
                meta.set_icon(icon);
            }
            let tags = &manifest.metadata.tags;
            let mut tags_builder = meta.reborrow().init_tags(tags.len() as u32);
            for (i, tag) in tags.iter().enumerate() {
                tags_builder.set(i as u32, tag);
            }
        }

        // Requirements
        {
            let mut reqs = builder.reborrow().init_requirements();
            if let Some(vram_str) = &manifest.requirements.vram_min {
                if let Ok(bytes) = parse_memory_string(vram_str) {
                    reqs.set_vram_min_bytes(bytes);
                }
            }
            if let Some(vram_str) = &manifest.requirements.vram_recommended {
                if let Ok(bytes) = parse_memory_string(vram_str) {
                    reqs.set_vram_recommended_bytes(bytes);
                }
            }
            if let Some(disk_str) = &manifest.requirements.disk {
                if let Ok(bytes) = parse_memory_string(disk_str) {
                    reqs.set_disk_bytes(bytes);
                }
            }
            let platforms = &manifest.requirements.platform;
            let mut plat_builder = reqs.reborrow().init_platform(platforms.len() as u32);
            for (i, p) in platforms.iter().enumerate() {
                let platform_str = match p {
                    Platform::DarwinArm64 => "darwin-arm64",
                    Platform::DarwinX86_64 => "darwin-x86_64",
                    Platform::LinuxAmd64 => "linux-amd64",
                    Platform::LinuxArm64 => "linux-arm64",
                };
                plat_builder.set(i as u32, platform_str);
            }
            let deps = &manifest.requirements.dependencies;
            let mut deps_builder = reqs.reborrow().init_dependencies(deps.len() as u32);
            for (i, dep) in deps.iter().enumerate() {
                deps_builder.set(i as u32, dep);
            }
        }

        // Execution
        {
            let mut exec = builder.reborrow().init_execution();
            exec.set_runtime(runtime_type_to_capnp(manifest.execution.runtime.clone()));
            exec.set_entrypoint(&manifest.execution.entrypoint);
            if let Some(port) = manifest.execution.port {
                exec.set_port(port);
            }
            if let Some(health) = &manifest.execution.health_check {
                exec.set_health_check(health);
            }
            exec.set_startup_timeout(manifest.execution.startup_timeout);
            let env = &manifest.execution.env;
            let mut env_builder = exec.reborrow().init_env(env.len() as u32);
            for (i, (k, v)) in env.iter().enumerate() {
                let mut entry = env_builder.reborrow().get(i as u32);
                entry.set_key(k);
                entry.set_value(v);
            }
        }

        // Storage
        {
            let mut storage = builder.reborrow().init_storage();
            storage.set_use_thin_provisioning(manifest.storage.use_thin_provisioning);
            let volumes = &manifest.storage.volumes;
            let mut vol_builder = storage.reborrow().init_volumes(volumes.len() as u32);
            for (i, vol) in volumes.iter().enumerate() {
                let mut v = vol_builder.reborrow().get(i as u32);
                v.set_name(&vol.name);
                v.set_mount_path(&vol.mount_path);
                v.set_read_only(vol.read_only);
                v.set_size_bytes(vol.size_bytes);
                v.set_use_thin(vol.use_thin.unwrap_or(false));
                v.set_encrypted(vol.encrypted);
            }
        }

        // Routing
        {
            let mut routing = builder.reborrow().init_routing();
            routing.set_weight(route_weight_to_capnp(manifest.routing.weight));
            routing.set_fallback_to_cloud(manifest.routing.fallback_to_cloud);
            if let Some(cloud_capsule) = &manifest.routing.cloud_capsule {
                routing.set_cloud_capsule(cloud_capsule);
            }
        }

        // Network (optional)
        if let Some(net) = &manifest.network {
            let mut network = builder.reborrow().init_network();
            let domains = &net.egress_allow;
            let mut domains_builder = network.reborrow().init_egress_allow(domains.len() as u32);
            for (i, domain) in domains.iter().enumerate() {
                domains_builder.set(i as u32, domain);
            }
            let rules = &net.egress_id_allow;
            let mut rules_builder = network.reborrow().init_egress_id_allow(rules.len() as u32);
            for (i, rule) in rules.iter().enumerate() {
                let mut rule_builder = rules_builder.reborrow().get(i as u32);
                rule_builder.set_type(egress_id_type_to_capnp(rule.rule_type.clone()));
                rule_builder.set_value(&rule.value);
            }
        }

        // Model (optional)
        if let Some(model) = &manifest.model {
            let mut model_builder = builder.reborrow().init_model();
            if let Some(source) = &model.source {
                model_builder.set_source(source);
            }
            if let Some(q) = &model.quantization {
                model_builder.set_quantization(quantization_to_capnp(*q));
            }
        }

        // Transparency (optional)
        if let Some(transparency) = &manifest.transparency {
            let mut trans_builder = builder.reborrow().init_transparency();
            trans_builder.set_level(transparency_level_to_capnp(transparency.level));
            let patterns = &transparency.allowed_binaries;
            let mut patterns_builder = trans_builder
                .reborrow()
                .init_allowed_binaries(patterns.len() as u32);
            for (i, pattern) in patterns.iter().enumerate() {
                patterns_builder.set(i as u32, pattern);
            }
        }
    }

    let bytes = capnp::serialize::write_message_to_words(&message);
    Ok(bytes)
}
