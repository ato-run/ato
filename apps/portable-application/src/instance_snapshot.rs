use std::collections::{BTreeMap, BTreeSet};

use ato_computation::ContentRef;
use ato_formation::authoring::{
    BOUND_CONTRACT_SCHEMA, BoundContract, BoundRequirement, INSTANCE_SNAPSHOT_CONTRACT_VERIFIER,
};
use ato_objects::{
    PORTABLE_APPLICATION_BUNDLE_VERSION_V4, PortableApplicationBundle,
    PortableBundleObjectDescriptor, PortableBundleObjectKind, PortableBundlePayload,
    encode_portable_application_bundle, reachable_portable_application_objects,
};
use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::{
    PortableApplicationError, bundle_sha256, parse_ref, portable_reference_registry, profile,
    structured, validate_all_derivations,
};

pub const INSTANCE_SNAPSHOT_SCHEMA: &str = "ato.portable-instance-snapshot/1";
pub const BROWSER_INSTANCE_STATE_PROTOCOL: &str = "ato.browser-instance-state@1";
pub const DATA_JSON_PROTOCOL: &str = "ato.data.json@1";
pub const ASSET_ALIAS_URI_PREFIX: &str = "ato-asset-alias://";
const MAX_DATA_JSON_BYTES: usize = 1024 * 1024;
const MAX_BROWSER_STATE_BYTES: usize = 16 * 1024 * 1024;
const MAX_BROWSER_STATE_ITEMS: usize = 4096;
const MAX_ASSET_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceSnapshotV1 {
    pub schema: String,
    pub resources: Vec<InstanceSnapshotResourceV1>,
    pub assets: Vec<InstanceSnapshotAssetV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub asset_bindings: Vec<InstanceSnapshotAssetBindingV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceSnapshotResourceV1 {
    pub slot: String,
    pub protocol: String,
    pub content_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceSnapshotAssetV1 {
    pub alias: String,
    pub content_ref: String,
    pub filename: String,
    pub content_type: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceSnapshotAssetBindingV1 {
    pub alias: String,
    pub resource_slot: String,
    pub location: InstanceSnapshotAssetLocationV1,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InstanceSnapshotAssetLocationV1 {
    DataJson {
        pointer: String,
    },
    BrowserLocalStorage {
        key: String,
        pointer: String,
        json_encoded: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserStateV1 {
    version: u32,
    local_storage: Vec<BrowserStateEntryV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserStateEntryV1 {
    key: String,
    value: String,
}

pub(crate) fn validate_snapshot(
    snapshot: &InstanceSnapshotV1,
) -> Result<Vec<ContentRef>, PortableApplicationError> {
    if snapshot.schema != INSTANCE_SNAPSHOT_SCHEMA {
        return Err(profile(format!(
            "unsupported Instance snapshot schema `{}`",
            snapshot.schema
        )));
    }
    let mut references = Vec::with_capacity(snapshot.resources.len() + snapshot.assets.len());
    let mut previous_slot = None;
    for resource in &snapshot.resources {
        if !valid_slot(&resource.slot)
            || !matches!(
                resource.protocol.as_str(),
                BROWSER_INSTANCE_STATE_PROTOCOL | DATA_JSON_PROTOCOL
            )
            || previous_slot.is_some_and(|previous: &str| previous >= resource.slot.as_str())
        {
            return Err(profile(
                "Instance snapshot resources must use a supported protocol and sorted unique slots",
            ));
        }
        previous_slot = Some(resource.slot.as_str());
        references.push(snapshot_ref(&resource.content_ref)?);
    }
    let mut previous_alias = None;
    for asset in &snapshot.assets {
        if asset.alias.is_empty()
            || asset.alias.len() > 128
            || asset.filename.is_empty()
            || asset.filename.len() > 255
            || asset.filename.contains('/')
            || asset.filename.contains('\\')
            || asset.filename.contains('\0')
            || asset.content_type.is_empty()
            || asset.content_type.len() > 255
            || asset.size > MAX_ASSET_BYTES
            || previous_alias.is_some_and(|previous: &str| previous >= asset.alias.as_str())
        {
            return Err(profile(
                "Instance snapshot Assets must have sorted unique aliases and safe metadata",
            ));
        }
        previous_alias = Some(asset.alias.as_str());
        references.push(snapshot_ref(&asset.content_ref)?);
    }
    let aliases = snapshot
        .assets
        .iter()
        .map(|asset| asset.alias.as_str())
        .collect::<BTreeSet<_>>();
    let resources = snapshot
        .resources
        .iter()
        .map(|resource| (resource.slot.as_str(), resource.protocol.as_str()))
        .collect::<BTreeMap<_, _>>();
    if snapshot
        .asset_bindings
        .windows(2)
        .any(|bindings| bindings[0] >= bindings[1])
    {
        return Err(profile(
            "Instance snapshot Asset bindings must be sorted and unique",
        ));
    }
    for binding in &snapshot.asset_bindings {
        if !aliases.contains(binding.alias.as_str()) {
            return Err(profile(format!(
                "Instance snapshot Asset binding references unknown alias `{}`",
                binding.alias
            )));
        }
        let expected_protocol = match &binding.location {
            InstanceSnapshotAssetLocationV1::DataJson { pointer } => {
                validate_json_pointer(pointer)?;
                DATA_JSON_PROTOCOL
            }
            InstanceSnapshotAssetLocationV1::BrowserLocalStorage {
                key,
                pointer,
                json_encoded,
            } => {
                if key.is_empty() || key.len() > 16 * 1024 {
                    return Err(profile(
                        "Instance snapshot browser Asset binding key is invalid",
                    ));
                }
                validate_json_pointer(pointer)?;
                if !json_encoded && !pointer.is_empty() {
                    return Err(profile(
                        "plain browser Asset binding cannot contain a JSON pointer",
                    ));
                }
                BROWSER_INSTANCE_STATE_PROTOCOL
            }
        };
        if resources.get(binding.resource_slot.as_str()).copied() != Some(expected_protocol) {
            return Err(profile(format!(
                "Instance snapshot Asset binding references incompatible resource `{}`",
                binding.resource_slot
            )));
        }
    }
    Ok(references)
}

fn validate_json_pointer(pointer: &str) -> Result<(), PortableApplicationError> {
    if pointer.len() > 4096 || (!pointer.is_empty() && !pointer.starts_with('/')) {
        return Err(profile(
            "Instance snapshot Asset binding JSON pointer is invalid",
        ));
    }
    let mut characters = pointer.chars();
    while let Some(character) = characters.next() {
        if character == '~' && !matches!(characters.next(), Some('0' | '1')) {
            return Err(profile(
                "Instance snapshot Asset binding JSON pointer is invalid",
            ));
        }
    }
    Ok(())
}

fn valid_slot(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && value.len() <= 128
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | '-')
        })
}

pub(crate) fn validate_snapshot_resource_bytes(
    protocol: &str,
    bytes: &[u8],
) -> Result<(), PortableApplicationError> {
    let limit = match protocol {
        DATA_JSON_PROTOCOL => MAX_DATA_JSON_BYTES,
        BROWSER_INSTANCE_STATE_PROTOCOL => MAX_BROWSER_STATE_BYTES,
        _ => {
            return Err(profile(format!(
                "unsupported snapshot protocol `{protocol}`"
            )));
        }
    };
    if bytes.len() > limit {
        return Err(profile(format!(
            "snapshot resource for `{protocol}` exceeds its byte limit"
        )));
    }
    if protocol == DATA_JSON_PROTOCOL {
        let value: serde_json::Value = serde_json::from_slice(bytes)?;
        if serde_jcs::to_vec(&value)? != bytes {
            return Err(profile("snapshot Data Resource JSON must be canonical"));
        }
        return Ok(());
    }
    let state: BrowserStateV1 = serde_json::from_slice(bytes)?;
    if state.version != 1 || state.local_storage.len() > MAX_BROWSER_STATE_ITEMS {
        return Err(profile(
            "snapshot browser state version or item count is invalid",
        ));
    }
    if state
        .local_storage
        .windows(2)
        .any(|entries| entries[0].key >= entries[1].key)
        || serde_jcs::to_vec(&state)? != bytes
    {
        return Err(profile(
            "snapshot browser state must be canonical with sorted unique keys",
        ));
    }
    Ok(())
}

pub fn decode_browser_state(
    bytes: &[u8],
) -> Result<BTreeMap<String, String>, PortableApplicationError> {
    validate_snapshot_resource_bytes(BROWSER_INSTANCE_STATE_PROTOCOL, bytes)?;
    let state: BrowserStateV1 = serde_json::from_slice(bytes)?;
    Ok(state
        .local_storage
        .into_iter()
        .map(|entry| (entry.key, entry.value))
        .collect())
}

pub fn encode_browser_state(
    local_storage: &BTreeMap<String, String>,
) -> Result<Vec<u8>, PortableApplicationError> {
    let state = BrowserStateV1 {
        version: 1,
        local_storage: local_storage
            .iter()
            .map(|(key, value)| BrowserStateEntryV1 {
                key: key.clone(),
                value: value.clone(),
            })
            .collect(),
    };
    let bytes = serde_jcs::to_vec(&state)?;
    validate_snapshot_resource_bytes(BROWSER_INSTANCE_STATE_PROTOCOL, &bytes)?;
    Ok(bytes)
}

fn alias_uri(alias: &str) -> String {
    format!("{ASSET_ALIAS_URI_PREFIX}{alias}")
}

fn count_alias_values(value: &serde_json::Value) -> Result<usize, PortableApplicationError> {
    match value {
        serde_json::Value::String(value) => {
            if value.starts_with(ASSET_ALIAS_URI_PREFIX) {
                if value.len() == ASSET_ALIAS_URI_PREFIX.len() {
                    return Err(profile(
                        "Instance snapshot contains an empty Asset alias URI",
                    ));
                }
                Ok(1)
            } else if value.contains(ASSET_ALIAS_URI_PREFIX) {
                Err(profile(
                    "Instance snapshot Asset alias URI must occupy a complete JSON string",
                ))
            } else {
                Ok(0)
            }
        }
        serde_json::Value::Array(values) => {
            values.iter().try_fold(
                0usize,
                |count, value| Ok(count + count_alias_values(value)?),
            )
        }
        serde_json::Value::Object(values) => {
            values.values().try_fold(
                0usize,
                |count, value| Ok(count + count_alias_values(value)?),
            )
        }
        _ => Ok(0),
    }
}

pub(crate) fn validate_asset_bindings(
    snapshot: &InstanceSnapshotV1,
    content: &BTreeMap<String, Vec<u8>>,
) -> Result<(), PortableApplicationError> {
    let resources = snapshot
        .resources
        .iter()
        .map(|resource| (resource.slot.as_str(), resource))
        .collect::<BTreeMap<_, _>>();
    let mut found_aliases = 0usize;
    for resource in &snapshot.resources {
        let bytes = content
            .get(&resource.content_ref)
            .ok_or_else(|| profile("snapshot resource content is missing"))?;
        if resource.protocol == DATA_JSON_PROTOCOL {
            let value: serde_json::Value = serde_json::from_slice(bytes)?;
            found_aliases += count_alias_values(&value)?;
        } else {
            let state: BrowserStateV1 = serde_json::from_slice(bytes)?;
            for entry in state.local_storage {
                if entry.value.starts_with(ASSET_ALIAS_URI_PREFIX) {
                    if entry.value.len() == ASSET_ALIAS_URI_PREFIX.len() {
                        return Err(profile(
                            "Instance snapshot contains an empty Asset alias URI",
                        ));
                    }
                    found_aliases += 1;
                } else if let Ok(value) = serde_json::from_str::<serde_json::Value>(&entry.value) {
                    found_aliases += count_alias_values(&value)?;
                } else if entry.value.contains(ASSET_ALIAS_URI_PREFIX) {
                    return Err(profile(
                        "Instance snapshot Asset alias URI must occupy a complete value",
                    ));
                }
            }
        }
    }
    for binding in &snapshot.asset_bindings {
        let resource = resources
            .get(binding.resource_slot.as_str())
            .ok_or_else(|| profile("snapshot Asset binding resource is missing"))?;
        let bytes = content
            .get(&resource.content_ref)
            .ok_or_else(|| profile("snapshot Asset binding content is missing"))?;
        let expected = alias_uri(&binding.alias);
        let actual = match &binding.location {
            InstanceSnapshotAssetLocationV1::DataJson { pointer } => {
                let value: serde_json::Value = serde_json::from_slice(bytes)?;
                value
                    .pointer(pointer)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            }
            InstanceSnapshotAssetLocationV1::BrowserLocalStorage {
                key,
                pointer,
                json_encoded,
            } => {
                let state: BrowserStateV1 = serde_json::from_slice(bytes)?;
                let value = state
                    .local_storage
                    .into_iter()
                    .find(|entry| entry.key == *key)
                    .map(|entry| entry.value)
                    .ok_or_else(|| profile("snapshot browser Asset binding key is missing"))?;
                if !json_encoded {
                    Some(value)
                } else {
                    serde_json::from_str::<serde_json::Value>(&value)
                        .ok()
                        .and_then(|value| {
                            value
                                .pointer(pointer)
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_owned)
                        })
                }
            }
        };
        if actual.as_deref() != Some(expected.as_str()) {
            return Err(profile(format!(
                "Instance snapshot Asset binding for `{}` does not resolve to its alias URI",
                binding.alias
            )));
        }
    }
    if found_aliases != snapshot.asset_bindings.len() {
        return Err(profile(
            "Instance snapshot contains an undeclared or multiply bound Asset alias URI",
        ));
    }
    Ok(())
}

pub(crate) fn rebind_snapshot_resource_assets(
    snapshot: &InstanceSnapshotV1,
    resource: &InstanceSnapshotResourceV1,
    bytes: &[u8],
    asset_uris: &BTreeMap<String, String>,
) -> Result<Vec<u8>, PortableApplicationError> {
    let bindings = snapshot
        .asset_bindings
        .iter()
        .filter(|binding| binding.resource_slot == resource.slot)
        .collect::<Vec<_>>();
    if bindings.is_empty() {
        return Ok(bytes.to_vec());
    }
    if resource.protocol == DATA_JSON_PROTOCOL {
        let mut value: serde_json::Value = serde_json::from_slice(bytes)?;
        for binding in bindings {
            let InstanceSnapshotAssetLocationV1::DataJson { pointer } = &binding.location else {
                return Err(profile("snapshot Asset binding protocol mismatch"));
            };
            let target = value
                .pointer_mut(pointer)
                .ok_or_else(|| profile("snapshot Asset binding JSON pointer is missing"))?;
            let expected = alias_uri(&binding.alias);
            if target.as_str() != Some(expected.as_str()) {
                return Err(profile("snapshot Asset binding value changed"));
            }
            *target = serde_json::Value::String(
                asset_uris
                    .get(&binding.alias)
                    .ok_or_else(|| profile("snapshot Asset alias was not materialized"))?
                    .clone(),
            );
        }
        return Ok(serde_jcs::to_vec(&value)?);
    }

    let mut state: BrowserStateV1 = serde_json::from_slice(bytes)?;
    for binding in bindings {
        let InstanceSnapshotAssetLocationV1::BrowserLocalStorage {
            key,
            pointer,
            json_encoded,
        } = &binding.location
        else {
            return Err(profile("snapshot Asset binding protocol mismatch"));
        };
        let entry = state
            .local_storage
            .iter_mut()
            .find(|entry| entry.key == *key)
            .ok_or_else(|| profile("snapshot browser Asset binding key is missing"))?;
        let expected = alias_uri(&binding.alias);
        let replacement = asset_uris
            .get(&binding.alias)
            .ok_or_else(|| profile("snapshot Asset alias was not materialized"))?;
        if !json_encoded {
            if entry.value != expected {
                return Err(profile("snapshot browser Asset binding value changed"));
            }
            entry.value = replacement.clone();
        } else {
            let mut value: serde_json::Value = serde_json::from_str(&entry.value)?;
            let target = value
                .pointer_mut(pointer)
                .ok_or_else(|| profile("snapshot browser Asset binding JSON pointer is missing"))?;
            if target.as_str() != Some(expected.as_str()) {
                return Err(profile("snapshot browser Asset binding value changed"));
            }
            *target = serde_json::Value::String(replacement.clone());
            entry.value = serde_jcs::to_string(&value)?;
        }
    }
    Ok(serde_jcs::to_vec(&state)?)
}

pub(crate) fn capture_snapshot_resource_assets(
    snapshot: &InstanceSnapshotV1,
    resource: &InstanceSnapshotResourceV1,
    bytes: &[u8],
    asset_uris: &BTreeMap<String, String>,
) -> Result<Vec<u8>, PortableApplicationError> {
    let bindings = snapshot
        .asset_bindings
        .iter()
        .filter(|binding| binding.resource_slot == resource.slot)
        .collect::<Vec<_>>();
    if bindings.is_empty() {
        validate_snapshot_resource_bytes(&resource.protocol, bytes)?;
        return Ok(bytes.to_vec());
    }
    if resource.protocol == DATA_JSON_PROTOCOL {
        let mut value: serde_json::Value = serde_json::from_slice(bytes)?;
        for binding in bindings {
            let InstanceSnapshotAssetLocationV1::DataJson { pointer } = &binding.location else {
                return Err(profile("snapshot Asset binding protocol mismatch"));
            };
            let target = value
                .pointer_mut(pointer)
                .ok_or_else(|| profile("snapshot Asset binding JSON pointer is missing"))?;
            let local_uri = asset_uris
                .get(&binding.alias)
                .ok_or_else(|| profile("snapshot Asset alias was not materialized"))?;
            if target.as_str() != Some(local_uri.as_str()) {
                return Err(profile("local snapshot Asset binding value changed"));
            }
            *target = serde_json::Value::String(alias_uri(&binding.alias));
        }
        let bytes = serde_jcs::to_vec(&value)?;
        validate_snapshot_resource_bytes(&resource.protocol, &bytes)?;
        return Ok(bytes);
    }

    let mut state: BrowserStateV1 = serde_json::from_slice(bytes)?;
    for binding in bindings {
        let InstanceSnapshotAssetLocationV1::BrowserLocalStorage {
            key,
            pointer,
            json_encoded,
        } = &binding.location
        else {
            return Err(profile("snapshot Asset binding protocol mismatch"));
        };
        let entry = state
            .local_storage
            .iter_mut()
            .find(|entry| entry.key == *key)
            .ok_or_else(|| profile("snapshot browser Asset binding key is missing"))?;
        let local_uri = asset_uris
            .get(&binding.alias)
            .ok_or_else(|| profile("snapshot Asset alias was not materialized"))?;
        let alias_uri = alias_uri(&binding.alias);
        if !json_encoded {
            if entry.value != *local_uri {
                return Err(profile(
                    "local snapshot browser Asset binding value changed",
                ));
            }
            entry.value = alias_uri;
        } else {
            let mut value: serde_json::Value = serde_json::from_str(&entry.value)?;
            let target = value
                .pointer_mut(pointer)
                .ok_or_else(|| profile("snapshot browser Asset binding JSON pointer is missing"))?;
            if target.as_str() != Some(local_uri.as_str()) {
                return Err(profile(
                    "local snapshot browser Asset binding value changed",
                ));
            }
            *target = serde_json::Value::String(alias_uri);
            entry.value = serde_jcs::to_string(&value)?;
        }
    }
    let bytes = serde_jcs::to_vec(&state)?;
    validate_snapshot_resource_bytes(&resource.protocol, &bytes)?;
    Ok(bytes)
}

pub(crate) fn validated_snapshot(
    bundle: &PortableApplicationBundle,
) -> Result<Option<(String, InstanceSnapshotV1)>, PortableApplicationError> {
    let Some(reference) = bundle.index.instance_snapshot_ref.as_deref() else {
        return Ok(None);
    };
    let reference = snapshot_ref(reference)?;
    let snapshot: InstanceSnapshotV1 = structured(bundle, &reference, INSTANCE_SNAPSHOT_SCHEMA)?;
    validate_snapshot(&snapshot)?;
    Ok(Some((reference.to_string(), snapshot)))
}

fn snapshot_ref(value: &str) -> Result<ContentRef, PortableApplicationError> {
    let reference = ContentRef::parse(value).map_err(|error| profile(error.to_string()))?;
    if reference.algorithm() != "sha256" || reference.to_string() != value {
        return Err(profile(format!(
            "Instance snapshot content reference must be canonical SHA-256: `{value}`"
        )));
    }
    Ok(reference)
}

/// Bind or replace a portable saved-state snapshot in a v4 bundle and mint K.
///
/// Derivations are not rewritten: only the snapshot object, its embedded blob
/// closure, and the Contract requirement that identifies that snapshot change.
pub fn attach_instance_snapshot(
    source: &PortableApplicationBundle,
    snapshot: InstanceSnapshotV1,
    content: &BTreeMap<String, Vec<u8>>,
) -> Result<(Vec<u8>, PortableApplicationBundle), PortableApplicationError> {
    validate_all_derivations(source)?;
    if source.index.version != PORTABLE_APPLICATION_BUNDLE_VERSION_V4 {
        return Err(profile("Instance snapshots require portable bundle v4"));
    }
    let references = validate_snapshot(&snapshot)?;
    if references.is_empty() {
        return Err(profile("Instance snapshot is empty"));
    }
    let expected = references
        .iter()
        .map(ToString::to_string)
        .collect::<BTreeSet<_>>();
    if content.keys().cloned().collect::<BTreeSet<_>>() != expected {
        return Err(profile(
            "snapshot content map must equal the declared resource/Asset closure",
        ));
    }
    for resource in &snapshot.resources {
        validate_snapshot_resource_bytes(
            &resource.protocol,
            content
                .get(&resource.content_ref)
                .ok_or_else(|| profile("snapshot resource content is missing"))?,
        )?;
    }
    validate_asset_bindings(&snapshot, content)?;

    let old_contract_ref = parse_ref(&source.index.root_contract_ref, "root_contract_ref")?;
    let mut contract: BoundContract = structured(source, &old_contract_ref, BOUND_CONTRACT_SCHEMA)?;
    let snapshot_requirement_id = contract
        .requirements
        .iter()
        .find(|requirement| requirement.verifier == INSTANCE_SNAPSHOT_CONTRACT_VERIFIER)
        .map_or_else(
            || "instance-snapshot".to_owned(),
            |requirement| requirement.id.clone(),
        );
    if contract.requirements.iter().any(|requirement| {
        requirement.id == snapshot_requirement_id
            && requirement.verifier != INSTANCE_SNAPSHOT_CONTRACT_VERIFIER
    }) {
        return Err(profile(
            "Contract already contains requirement id `instance-snapshot`",
        ));
    }
    contract
        .requirements
        .retain(|requirement| requirement.verifier != INSTANCE_SNAPSHOT_CONTRACT_VERIFIER);

    let mut bundle = source.clone();
    for (reference, bytes) in content {
        if bundle_sha256(bytes) != *reference {
            return Err(profile(format!(
                "snapshot bytes do not match declared reference `{reference}`"
            )));
        }
        insert_object(&mut bundle, reference, None, bytes)?;
    }
    let snapshot_bytes = serde_jcs::to_vec(&snapshot)?;
    let snapshot_ref = bundle_sha256(&snapshot_bytes);
    insert_object(
        &mut bundle,
        &snapshot_ref,
        Some(INSTANCE_SNAPSHOT_SCHEMA),
        &snapshot_bytes,
    )?;

    contract.requirements.push(BoundRequirement {
        id: snapshot_requirement_id,
        verifier: INSTANCE_SNAPSHOT_CONTRACT_VERIFIER.to_owned(),
        port: None,
        method: None,
        path: None,
        status: None,
        body_digest: None,
        input: None,
        digest: Some(snapshot_ref.clone()),
    });
    contract
        .requirements
        .sort_by(|left, right| left.id.cmp(&right.id));
    let contract_bytes = serde_jcs::to_vec(&contract)?;
    let contract_ref = bundle_sha256(&contract_bytes);
    remove_object(&mut bundle, old_contract_ref.as_str());
    insert_object(
        &mut bundle,
        &contract_ref,
        Some(BOUND_CONTRACT_SCHEMA),
        &contract_bytes,
    )?;
    bundle.index.root_contract_ref = contract_ref;
    bundle.index.instance_snapshot_ref = Some(snapshot_ref);
    bundle
        .index
        .objects
        .sort_by(|left, right| left.reference.cmp(&right.reference));
    bundle
        .payloads
        .sort_by(|left, right| left.reference.cmp(&right.reference));
    prune_unreachable_objects(&mut bundle)?;
    validate_all_derivations(&bundle)?;
    let bytes = encode_portable_application_bundle(&bundle)?;
    Ok((bytes, bundle))
}

fn prune_unreachable_objects(
    bundle: &mut PortableApplicationBundle,
) -> Result<(), PortableApplicationError> {
    let registry = portable_reference_registry()?;
    let reachable = reachable_portable_application_objects(bundle, &registry)?;
    let reachable = reachable
        .iter()
        .map(ToString::to_string)
        .collect::<BTreeSet<_>>();
    bundle
        .index
        .objects
        .retain(|descriptor| reachable.contains(descriptor.reference.as_str()));
    bundle
        .payloads
        .retain(|payload| reachable.contains(payload.reference.as_str()));
    if let Some(portability) = bundle.portability.as_mut() {
        portability
            .external_objects
            .retain(|external| reachable.contains(external.reference.as_str()));
    }
    Ok(())
}

fn insert_object(
    bundle: &mut PortableApplicationBundle,
    reference: &str,
    schema: Option<&str>,
    bytes: &[u8],
) -> Result<(), PortableApplicationError> {
    let expected_kind = if schema.is_some() {
        PortableBundleObjectKind::Structured
    } else {
        PortableBundleObjectKind::Blob
    };
    if let Some(descriptor) = bundle
        .index
        .objects
        .iter()
        .find(|descriptor| descriptor.reference == reference)
    {
        if descriptor.kind != expected_kind
            || descriptor.schema.as_deref() != schema
            || descriptor.size != bytes.len() as u64
        {
            return Err(profile(format!(
                "bundle already declares `{reference}` with incompatible object metadata"
            )));
        }
        if let Some(payload) = bundle
            .payloads
            .iter()
            .find(|payload| payload.reference == reference)
        {
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(&payload.bytes)
                .map_err(|_| profile(format!("invalid payload for `{reference}`")))?;
            if decoded != bytes {
                return Err(profile(format!(
                    "bundle already contains different bytes for `{reference}`"
                )));
            }
            return Ok(());
        }
        if let Some(portability) = bundle.portability.as_mut() {
            portability
                .external_objects
                .retain(|external| external.reference != reference);
        }
        bundle.payloads.push(PortableBundlePayload {
            reference: reference.to_owned(),
            bytes: base64::engine::general_purpose::STANDARD.encode(bytes),
        });
        return Ok(());
    }
    bundle.index.objects.push(PortableBundleObjectDescriptor {
        reference: reference.to_owned(),
        kind: expected_kind,
        schema: schema.map(ToOwned::to_owned),
        size: bytes.len() as u64,
    });
    bundle.payloads.push(PortableBundlePayload {
        reference: reference.to_owned(),
        bytes: base64::engine::general_purpose::STANDARD.encode(bytes),
    });
    Ok(())
}

fn remove_object(bundle: &mut PortableApplicationBundle, reference: &str) {
    bundle
        .index
        .objects
        .retain(|descriptor| descriptor.reference != reference);
    bundle
        .payloads
        .retain(|payload| payload.reference != reference);
}
