//! Repack a verified semantic graph without rebinding K or D.

use std::collections::BTreeMap;

use ato_objects::{
    PORTABLE_APPLICATION_BUNDLE_VERSION, PORTABLE_APPLICATION_BUNDLE_VERSION_V4,
    PORTABLE_APPLICATION_PROFILE_V2, PortableApplicationBundle, PortableBundleObjectKind,
    PortableDependencyProfile, PortableDependencyTransport, PortableExternalObject,
    PortableOciArchive, encode_portable_application_bundle,
};

use crate::{PortableApplicationError, PortableRealizationKind, profile, validate_all_derivations};

pub fn repack_portable_dependencies(
    original: &PortableApplicationBundle,
    policy: PortableDependencyProfile,
    external_sources: &BTreeMap<String, Vec<String>>,
) -> Result<(Vec<u8>, PortableApplicationBundle), PortableApplicationError> {
    repack_portable_dependencies_with_archives(original, policy, external_sources, &[])
}

pub fn repack_portable_dependencies_with_archives(
    original: &PortableApplicationBundle,
    policy: PortableDependencyProfile,
    external_sources: &BTreeMap<String, Vec<String>>,
    oci_archives: &[PortableOciArchive],
) -> Result<(Vec<u8>, PortableApplicationBundle), PortableApplicationError> {
    match original.index.version {
        PORTABLE_APPLICATION_BUNDLE_VERSION if original.portability.is_none() => {}
        PORTABLE_APPLICATION_BUNDLE_VERSION_V4 if original.portability.is_some() => {}
        _ => {
            return Err(profile(
                "dependency repack requires a validated portable application v3 or v4 bundle",
            ));
        }
    }
    let before = validate_all_derivations(original)?;
    if policy == PortableDependencyProfile::Offline
        && before
            .iter()
            .filter(|route| route.realization == PortableRealizationKind::OciContainer)
            .any(|route| {
                !oci_archives.iter().any(|archive| {
                    route.derivation.runtimes.get(crate::OCI_IMAGE_RUNTIME) == Some(&archive.image)
                        && route.derivation.runtimes.get(crate::OCI_PLATFORM_RUNTIME)
                            == Some(&archive.platform)
                })
            })
    {
        return Err(profile(
            "offline OCI export requires a verified embedded registry manifest and layer closure",
        ));
    }
    if policy == PortableDependencyProfile::Offline && !external_sources.is_empty() {
        return Err(profile(
            "offline export cannot externalize dependency objects",
        ));
    }

    for reference in external_sources.keys() {
        let descriptor = original
            .index
            .objects
            .iter()
            .find(|descriptor| &descriptor.reference == reference)
            .ok_or_else(|| profile(format!("external object `{reference}` is not declared")))?;
        if descriptor.kind != PortableBundleObjectKind::Blob {
            return Err(profile(format!(
                "external object `{reference}` is not a blob"
            )));
        }
    }
    let mut repacked = original.clone();
    repacked.index.version = PORTABLE_APPLICATION_BUNDLE_VERSION_V4;
    repacked.index.profile = PORTABLE_APPLICATION_PROFILE_V2.to_owned();
    repacked
        .payloads
        .retain(|payload| !external_sources.contains_key(&payload.reference));
    let mut oci_archives = oci_archives.to_vec();
    oci_archives.sort_by(|left, right| left.image.cmp(&right.image));
    repacked.portability = Some(PortableDependencyTransport {
        profile: policy,
        external_objects: external_sources
            .iter()
            .map(|(reference, sources)| PortableExternalObject {
                reference: reference.clone(),
                sources: sources.clone(),
            })
            .collect(),
        oci_archives,
    });
    let bytes = encode_portable_application_bundle(&repacked)?;
    let after = validate_all_derivations(&repacked)?;
    if before
        .iter()
        .map(|route| (&route.contract_ref, &route.derivation_ref))
        .collect::<Vec<_>>()
        != after
            .iter()
            .map(|route| (&route.contract_ref, &route.derivation_ref))
            .collect::<Vec<_>>()
    {
        return Err(profile(
            "dependency packing changed ContractRef or DerivationRef",
        ));
    }
    Ok((bytes, repacked))
}
