//! Build a portable export from an authenticated Hosted capture.
//!
//! The API owns authorization and capture fencing. This module remains the
//! semantic authority: it recomputes every content reference, binds saved data
//! into K, and proves that dependency packing did not change K or D.

use std::collections::BTreeMap;

use ato_objects::{
    CapsuleBundleDocument, PORTABLE_APPLICATION_BUNDLE_VERSION, PortableApplicationBundle,
    PortableDependencyProfile, PortableOciArchive, decode_capsule_bundle_document,
};
use serde::{Deserialize, Serialize};

use crate::instance_snapshot::{
    INSTANCE_SNAPSHOT_SCHEMA, InstanceSnapshotAssetBindingV1, InstanceSnapshotAssetV1,
    InstanceSnapshotResourceV1, InstanceSnapshotV1, attach_instance_snapshot,
};
use crate::portability_export::repack_portable_dependencies_with_archives;
use crate::{PortableApplicationError, bundle_sha256, profile};

pub const HOSTED_CAPTURE_SCHEMA: &str = "ato.portable-instance-capture/1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedInstanceCaptureV1 {
    pub schema: String,
    pub resources: Vec<HostedCaptureResourceV1>,
    pub assets: Vec<HostedCaptureAssetV1>,
    #[serde(default)]
    pub asset_bindings: Vec<InstanceSnapshotAssetBindingV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedCaptureResourceV1 {
    pub object_id: String,
    pub slot: String,
    pub protocol: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedCaptureAssetV1 {
    pub object_id: String,
    pub alias: String,
    pub filename: String,
    pub content_type: String,
    pub size: u64,
}

pub fn portable_bundle_from_bytes(
    bytes: &[u8],
) -> Result<PortableApplicationBundle, PortableApplicationError> {
    match decode_capsule_bundle_document(bytes).map_err(|error| profile(error.to_string()))? {
        CapsuleBundleDocument::PortableApplicationV3(bundle)
        | CapsuleBundleDocument::PortableApplicationV4(bundle) => Ok(bundle),
        CapsuleBundleDocument::ComputationV2(_) => Err(profile(
            "Hosted export requires a portable application v3 or v4 bundle",
        )),
    }
}

/// Attach a captured saved-data point, then apply only the requested transport
/// policy. `captured_objects` is keyed by the opaque capture object id; those
/// ids never enter the resulting Capsule.
pub fn build_hosted_export(
    source: &PortableApplicationBundle,
    capture: Option<&HostedInstanceCaptureV1>,
    captured_objects: &BTreeMap<String, Vec<u8>>,
    dependency_profile: PortableDependencyProfile,
    external_sources: &BTreeMap<String, Vec<String>>,
    oci_archives: &[PortableOciArchive],
) -> Result<(Vec<u8>, PortableApplicationBundle), PortableApplicationError> {
    let with_snapshot = match capture {
        None => {
            if !captured_objects.is_empty() {
                return Err(profile("app-only export received saved-data objects"));
            }
            source.clone()
        }
        Some(capture) => {
            if capture.schema != HOSTED_CAPTURE_SCHEMA {
                return Err(profile(format!(
                    "unsupported Hosted capture schema `{}`",
                    capture.schema
                )));
            }
            let declared = capture
                .resources
                .iter()
                .map(|resource| resource.object_id.as_str())
                .chain(capture.assets.iter().map(|asset| asset.object_id.as_str()))
                .collect::<std::collections::BTreeSet<_>>();
            if declared.len() != capture.resources.len() + capture.assets.len()
                || declared
                    != captured_objects
                        .keys()
                        .map(String::as_str)
                        .collect::<std::collections::BTreeSet<_>>()
            {
                return Err(profile(
                    "Hosted capture object set must exactly match its descriptor",
                ));
            }

            let mut content = BTreeMap::new();
            let resources = capture
                .resources
                .iter()
                .map(|resource| {
                    let bytes = captured_objects.get(&resource.object_id).ok_or_else(|| {
                        profile(format!(
                            "capture object `{}` is missing",
                            resource.object_id
                        ))
                    })?;
                    if bytes.len() as u64 != resource.size {
                        return Err(profile(format!(
                            "capture object `{}` changed size",
                            resource.object_id
                        )));
                    }
                    let content_ref = bundle_sha256(bytes);
                    content.insert(content_ref.clone(), bytes.clone());
                    Ok(InstanceSnapshotResourceV1 {
                        slot: resource.slot.clone(),
                        protocol: resource.protocol.clone(),
                        content_ref,
                    })
                })
                .collect::<Result<Vec<_>, PortableApplicationError>>()?;
            let assets = capture
                .assets
                .iter()
                .map(|asset| {
                    let bytes = captured_objects.get(&asset.object_id).ok_or_else(|| {
                        profile(format!("capture object `{}` is missing", asset.object_id))
                    })?;
                    if bytes.len() as u64 != asset.size {
                        return Err(profile(format!(
                            "capture object `{}` changed size",
                            asset.object_id
                        )));
                    }
                    let content_ref = bundle_sha256(bytes);
                    content.insert(content_ref.clone(), bytes.clone());
                    Ok(InstanceSnapshotAssetV1 {
                        alias: asset.alias.clone(),
                        content_ref,
                        filename: asset.filename.clone(),
                        content_type: asset.content_type.clone(),
                        size: asset.size,
                    })
                })
                .collect::<Result<Vec<_>, PortableApplicationError>>()?;
            let snapshot = InstanceSnapshotV1 {
                schema: INSTANCE_SNAPSHOT_SCHEMA.to_owned(),
                resources,
                assets,
                asset_bindings: capture.asset_bindings.clone(),
            };
            let snapshot_source = if source.index.version == PORTABLE_APPLICATION_BUNDLE_VERSION {
                repack_portable_dependencies_with_archives(
                    source,
                    PortableDependencyProfile::Cached,
                    &BTreeMap::new(),
                    &[],
                )?
                .1
            } else {
                source.clone()
            };
            attach_instance_snapshot(&snapshot_source, snapshot, &content)?.1
        }
    };

    repack_portable_dependencies_with_archives(
        &with_snapshot,
        dependency_profile,
        external_sources,
        oci_archives,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_static_bundle;
    use crate::instance_snapshot::{
        InstanceSnapshotAssetBindingV1, InstanceSnapshotAssetLocationV1,
    };
    use std::path::Path;

    fn source_bundle() -> PortableApplicationBundle {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-static-k");
        let (_, v3) = build_static_bundle(&source, "Todo").unwrap();
        repack_portable_dependencies_with_archives(
            &v3,
            PortableDependencyProfile::Cached,
            &BTreeMap::new(),
            &[],
        )
        .unwrap()
        .1
    }

    #[test]
    fn saved_capture_changes_k_but_preserves_every_derivation() {
        let source = source_bundle();
        let capture = HostedInstanceCaptureV1 {
            schema: HOSTED_CAPTURE_SCHEMA.to_owned(),
            resources: vec![HostedCaptureResourceV1 {
                object_id: "obj-1".to_owned(),
                slot: "browser".to_owned(),
                protocol: "ato.browser-instance-state@1".to_owned(),
                size: 56,
            }],
            assets: vec![HostedCaptureAssetV1 {
                object_id: "obj-2".to_owned(),
                alias: "asset-0001".to_owned(),
                filename: "photo.jpg".to_owned(),
                content_type: "image/jpeg".to_owned(),
                size: 5,
            }],
            asset_bindings: vec![
                InstanceSnapshotAssetBindingV1 {
                    alias: "asset-0001".to_owned(),
                    resource_slot: "browser".to_owned(),
                    location: InstanceSnapshotAssetLocationV1::BrowserLocalStorage {
                        key: "direct-photo".to_owned(),
                        pointer: String::new(),
                        json_encoded: false,
                    },
                },
                InstanceSnapshotAssetBindingV1 {
                    alias: "asset-0001".to_owned(),
                    resource_slot: "browser".to_owned(),
                    location: InstanceSnapshotAssetLocationV1::BrowserLocalStorage {
                        key: "json-photo".to_owned(),
                        pointer: String::new(),
                        json_encoded: true,
                    },
                },
            ],
        };
        let state = br#"{"local_storage":[{"key":"direct-photo","value":"ato-asset-alias://asset-0001"},{"key":"json-photo","value":"\"ato-asset-alias://asset-0001\""}],"version":1}"#;
        let mut capture = capture;
        capture.resources[0].size = state.len() as u64;
        let (_, exported) = build_hosted_export(
            &source,
            Some(&capture),
            &BTreeMap::from([
                ("obj-1".to_owned(), state.to_vec()),
                ("obj-2".to_owned(), b"photo".to_vec()),
            ]),
            PortableDependencyProfile::Cached,
            &BTreeMap::new(),
            &[],
        )
        .unwrap();
        assert_ne!(
            source.index.root_contract_ref,
            exported.index.root_contract_ref
        );
        assert_eq!(source.index.derivations, exported.index.derivations);
        assert!(exported.index.instance_snapshot_ref.is_some());
    }

    #[test]
    fn saved_capture_rejects_an_unbound_asset_alias_uri() {
        let source = source_bundle();
        let state = br#"{"local_storage":[{"key":"photo","value":"ato-asset-alias://asset-0001"}],"version":1}"#;
        let capture = HostedInstanceCaptureV1 {
            schema: HOSTED_CAPTURE_SCHEMA.to_owned(),
            resources: vec![HostedCaptureResourceV1 {
                object_id: "obj-1".to_owned(),
                slot: "browser".to_owned(),
                protocol: "ato.browser-instance-state@1".to_owned(),
                size: state.len() as u64,
            }],
            assets: vec![HostedCaptureAssetV1 {
                object_id: "obj-2".to_owned(),
                alias: "asset-0001".to_owned(),
                filename: "photo.jpg".to_owned(),
                content_type: "image/jpeg".to_owned(),
                size: 5,
            }],
            asset_bindings: vec![],
        };
        let error = build_hosted_export(
            &source,
            Some(&capture),
            &BTreeMap::from([
                ("obj-1".to_owned(), state.to_vec()),
                ("obj-2".to_owned(), b"photo".to_vec()),
            ]),
            PortableDependencyProfile::Cached,
            &BTreeMap::new(),
            &[],
        )
        .unwrap_err();
        assert!(error.to_string().contains("undeclared"));
    }

    #[test]
    fn app_only_repack_keeps_k_and_d() {
        let source = source_bundle();
        let (_, exported) = build_hosted_export(
            &source,
            None,
            &BTreeMap::new(),
            PortableDependencyProfile::Cached,
            &BTreeMap::new(),
            &[],
        )
        .unwrap();
        assert_eq!(
            source.index.root_contract_ref,
            exported.index.root_contract_ref
        );
        assert_eq!(source.index.derivations, exported.index.derivations);
    }
}
