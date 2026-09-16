//! Read-only export planning. It does not promise that an external dependency
//! is available without checking its immutable bytes.

use std::collections::{BTreeMap, BTreeSet};

use ato_objects::PortableApplicationBundle;
use clap::ValueEnum;
use serde::Serialize;

use crate::{
    OCI_IMAGE_RUNTIME, OCI_PLATFORM_RUNTIME, PYTHON_RUNTIME, PortableApplicationError,
    PortableRealizationKind, validate_all_derivations,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum PortableExportProfile {
    Thin,
    Cached,
    Offline,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PortableExportPlan {
    pub profile: PortableExportProfile,
    pub contract_ref: String,
    pub derivation_refs: Vec<String>,
    pub current_bundle_bytes: usize,
    pub estimated_export_bytes: Option<usize>,
    pub application_input_bytes: u64,
    pub python_wheel_bytes: u64,
    pub embedded_objects: usize,
    pub external_objects: usize,
    pub embedded_dependency_bytes: u64,
    pub external_dependency_bytes: u64,
    pub oci_images: Vec<String>,
    pub required_host_capabilities: Vec<String>,
    pub requires_network_on_clean_host: Option<bool>,
    pub existing_bundle_satisfies_profile: bool,
    pub blockers: Vec<String>,
}

/// Plan from the exact validated semantic closure. A v3 file already embeds
/// all workspace blobs, but its OCI image is a digest-pinned external runtime
/// dependency. Neither planning nor proposed packing changes K or D.
pub fn plan_portable_export(
    bundle: &PortableApplicationBundle,
    current_bundle_bytes: usize,
    profile: PortableExportProfile,
) -> Result<PortableExportPlan, PortableApplicationError> {
    let routes = validate_all_derivations(bundle)?;
    let mut input_sizes = BTreeMap::new();
    let mut wheel_refs = BTreeSet::new();
    for entry in &routes[0].tree.entries {
        input_sizes.insert(&entry.content_ref, entry.size);
        if entry.path.ends_with(".whl") {
            wheel_refs.insert(&entry.content_ref);
        }
    }
    let python_wheel_bytes = wheel_refs
        .iter()
        .map(|reference| input_sizes.get(reference).copied().unwrap_or(0))
        .sum();
    let application_input_bytes = input_sizes.values().sum::<u64>() - python_wheel_bytes;

    let mut oci_images = BTreeSet::new();
    let mut capabilities = BTreeSet::new();
    for route in &routes {
        match route.realization {
            PortableRealizationKind::StaticWeb => {}
            PortableRealizationKind::LocalProcess => {
                if let Some(version) = route.derivation.runtimes.get(PYTHON_RUNTIME) {
                    capabilities.insert(format!("python:{version}"));
                }
            }
            PortableRealizationKind::OciContainer => {
                if let Some(image) = route.derivation.runtimes.get(OCI_IMAGE_RUNTIME) {
                    oci_images.insert(image.clone());
                }
                if let Some(platform) = route.derivation.runtimes.get(OCI_PLATFORM_RUNTIME) {
                    capabilities.insert(format!("oci-runtime:{platform}"));
                }
            }
        }
    }

    let mut blockers = Vec::new();
    let external_wheels = if profile == PortableExportProfile::Thin {
        wheel_refs.len()
    } else {
        0
    };
    let embedded_objects = bundle.index.objects.len() - external_wheels;
    // An OCI image is outside the wire-v3 object graph and is counted as one
    // external dependency until a verified registry closure is attached.
    let external_objects = external_wheels + oci_images.len();
    let embedded_dependency_bytes = if external_wheels == 0 {
        python_wheel_bytes
    } else {
        0
    };
    let external_dependency_bytes = if external_wheels == 0 {
        0
    } else {
        python_wheel_bytes
    };
    let (estimated_export_bytes, requires_network_on_clean_host) = match profile {
        PortableExportProfile::Thin => {
            if !wheel_refs.is_empty() {
                blockers.push(format!(
                    "{} wheel sources must be discovered and matched by SHA-256 before thin export",
                    wheel_refs.len()
                ));
            }
            (None, Some(true))
        }
        PortableExportProfile::Cached => (Some(current_bundle_bytes), Some(!oci_images.is_empty())),
        PortableExportProfile::Offline => {
            if !oci_images.is_empty() {
                blockers.push(format!(
                    "{} OCI images lack verified embedded manifest/layer closure",
                    oci_images.len()
                ));
            }
            if blockers.is_empty() {
                (Some(current_bundle_bytes), Some(false))
            } else {
                (None, None)
            }
        }
    };
    Ok(PortableExportPlan {
        profile,
        contract_ref: routes[0].contract_ref.to_string(),
        derivation_refs: routes
            .iter()
            .map(|route| route.derivation_ref.to_string())
            .collect(),
        current_bundle_bytes,
        estimated_export_bytes,
        application_input_bytes,
        python_wheel_bytes,
        embedded_objects,
        external_objects,
        embedded_dependency_bytes,
        external_dependency_bytes,
        oci_images: oci_images.into_iter().collect(),
        required_host_capabilities: capabilities.into_iter().collect(),
        requires_network_on_clean_host,
        existing_bundle_satisfies_profile: blockers.is_empty(),
        blockers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        PortableDynamicBundleSpec, PortableExecutionSpec, PortableHttpRequirementSpec,
        build_dynamic_process_oci_bundle,
    };

    #[test]
    fn planning_never_changes_contract_or_derivation_identity() {
        let source = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("data.txt"), "fixed input").unwrap();
        let spec = PortableDynamicBundleSpec {
            title: "catalog".to_owned(),
            surface_path: "/".to_owned(),
            guest_port: 8000,
            process: PortableExecutionSpec {
                runtimes: BTreeMap::from([(PYTHON_RUNTIME.to_owned(), "3.12".to_owned())]),
                argv: vec!["python3".to_owned(), "app.py".to_owned()],
                cwd: ".".to_owned(),
                env: BTreeMap::new(),
            },
            oci: PortableExecutionSpec {
                runtimes: BTreeMap::from([
                    (
                        OCI_IMAGE_RUNTIME.to_owned(),
                        format!("docker.io/example/app@sha256:{}", "a".repeat(64)),
                    ),
                    (OCI_PLATFORM_RUNTIME.to_owned(), "linux/amd64".to_owned()),
                    (
                        crate::OCI_MEMORY_BYTES_RUNTIME.to_owned(),
                        "268435456".to_owned(),
                    ),
                    (crate::OCI_CPU_MILLIS_RUNTIME.to_owned(), "1000".to_owned()),
                    (crate::OCI_PIDS_LIMIT_RUNTIME.to_owned(), "128".to_owned()),
                ]),
                argv: vec!["serve".to_owned()],
                cwd: ".".to_owned(),
                env: BTreeMap::new(),
            },
            requirements: vec![PortableHttpRequirementSpec {
                id: "entry".to_owned(),
                path: "/".to_owned(),
                status: 200,
                body_digest: None,
            }],
        };
        let (bytes, bundle) = build_dynamic_process_oci_bundle(source.path(), &spec).unwrap();
        let cached =
            plan_portable_export(&bundle, bytes.len(), PortableExportProfile::Cached).unwrap();
        let thin = plan_portable_export(&bundle, bytes.len(), PortableExportProfile::Thin).unwrap();
        let offline =
            plan_portable_export(&bundle, bytes.len(), PortableExportProfile::Offline).unwrap();
        assert_eq!(cached.contract_ref, thin.contract_ref);
        assert_eq!(cached.contract_ref, offline.contract_ref);
        assert_eq!(cached.derivation_refs, thin.derivation_refs);
        assert_eq!(cached.derivation_refs, offline.derivation_refs);
        assert!(cached.existing_bundle_satisfies_profile);
        assert!(thin.existing_bundle_satisfies_profile);
        assert!(!offline.existing_bundle_satisfies_profile);
        assert_eq!(offline.requires_network_on_clean_host, None);
    }
}
