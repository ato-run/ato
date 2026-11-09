//! Cross-Derivation Instance state continuity.
//!
//! An Instance's saved data is independent of the Derivation (D) that runs it,
//! but D-independence of the saved *location* is not interoperability of the
//! saved *format*. This module supplies the declared mapping between a state
//! slot and the storage protocol/location it is saved through, the declared
//! save format and version per slot, and the one place that decides whether a
//! Derivation may restore saved data written by another.
//!
//! Three rules are enforced here and nowhere else:
//!
//! 1. Saved data declares its schema; a schema this build does not read is a
//!    named refusal, never a partial restore.
//! 2. A slot's format and version are compared with what the target Derivation
//!    declares. A difference is refused by name unless the target declares the
//!    saved version readable, in which case only an identity copy — the same
//!    bytes under the target's declaration — is permitted.
//! 3. Restore evidence is the digest recomputed from the bytes actually
//!    imported, not the presence of a snapshot reference.
//!
//! Nothing here knows about Instance ids, storage paths or processes: those are
//! local orchestration and never enter `ContractRef`/`DerivationRef`.

use std::collections::BTreeMap;
use std::fmt;

use ato_formation::authoring::{BoundState, STATE_FILESYSTEM_PROTOCOL};

use crate::instance_snapshot::{
    INSTANCE_SNAPSHOT_SCHEMA, InstanceSnapshotResourceV1, InstanceSnapshotV1, validate_snapshot,
};
use crate::{PortableApplicationError, bundle_sha256};

/// The format implicitly declared by every Derivation and every saved resource
/// that predates per-slot save declarations. Keeping it named means a route that
/// declares nothing keeps the behaviour — and the bytes — it had before.
pub const LEGACY_STATE_SAVE_FORMAT: &str = "ato.state.filesystem@1";
pub const LEGACY_STATE_SAVE_VERSION: u32 = 1;

/// The declared mapping for one state slot: which storage protocol and location
/// a saved resource must name, and which save format the slot is written in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetStateSlot {
    pub id: String,
    pub protocol: String,
    /// Where the route expects the slot: a fixed mount, or the host path handed
    /// to the workload through `ATO_STATE_PATH_<ID>`.
    pub location: StateSlotLocation,
    /// The save format this route declares, or the legacy format it implies.
    pub save_format: String,
    pub save_version: u32,
    /// Versions of `save_format` this route reads unchanged.
    pub compatible_versions: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateSlotLocation {
    /// A fixed mount at this absolute guest path.
    Mount { path: String },
    /// The route locates the slot through `ATO_STATE_PATH_<ID>`.
    InstancePath { variable: String },
}

/// One saved resource, resolved to the format/version its writer declared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedStateSlot {
    pub slot: String,
    pub protocol: String,
    pub save_format: String,
    pub save_version: u32,
}

/// A migration the target Derivation declared it can read unchanged. The bytes
/// are copied as they are; only the declaration they carry changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateMigration {
    pub slot: String,
    pub format: String,
    pub from_version: u32,
    pub to_version: u32,
}

/// What a target Derivation needs before it may restore this saved data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateCompatibility {
    Compatible,
    /// Explicit user approval is required before any saved byte is touched.
    MigrationRequired(Vec<StateMigration>),
}

/// Evidence that the caller obtained explicit user consent for a migration.
///
/// The type is deliberately opaque outside this crate: a caller cannot migrate
/// saved data without obtaining the approval this token represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateMigrationApproval {
    granted: bool,
}

impl StateMigrationApproval {
    /// Nothing has been approved. This is the default for every existing path.
    pub const fn not_granted() -> Self {
        Self { granted: false }
    }

    /// The user explicitly approved this migration.
    pub const fn granted() -> Self {
        Self { granted: true }
    }

    pub const fn is_granted(self) -> bool {
        self.granted
    }
}

/// A refusal with a stable, user-facing code and the refs involved. The code is
/// what tests and callers match on; the detail names the slot/format/version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateContinuityRefusal {
    pub code: &'static str,
    pub detail: String,
}

impl StateContinuityRefusal {
    pub fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for StateContinuityRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.detail)
    }
}

impl std::error::Error for StateContinuityRefusal {}

pub const STATE_CODE_SNAPSHOT_SCHEMA_UNKNOWN: &str = "state-snapshot-schema-unknown";
pub const STATE_CODE_SNAPSHOT_INVALID: &str = "state-snapshot-invalid";
pub const STATE_CODE_SLOT_UNDECLARED: &str = "state-slot-undeclared";
pub const STATE_CODE_PROTOCOL_MISMATCH: &str = "state-protocol-mismatch";
pub const STATE_CODE_FORMAT_INCOMPATIBLE: &str = "state-save-format-incompatible";
pub const STATE_CODE_VERSION_INCOMPATIBLE: &str = "state-save-version-incompatible";
pub const STATE_CODE_MIGRATION_APPROVAL_REQUIRED: &str = "state-migration-approval-required";
pub const STATE_CODE_IMPORT_CONTENT_MISSING: &str = "state-import-content-missing";
pub const STATE_CODE_IMPORT_DIGEST_MISMATCH: &str = "state-import-digest-mismatch";
pub const STATE_CODE_RESTORE_DIGEST_MISMATCH: &str = "state-restore-digest-mismatch";

/// Validate saved data on read: the schema this build reads, then its shape.
///
/// A saved-data schema this build does not carry is refused by name before any
/// content is read, so a newer producer never yields a partial restore.
pub fn validate_saved_data(snapshot: &InstanceSnapshotV1) -> Result<(), StateContinuityRefusal> {
    if snapshot.schema != INSTANCE_SNAPSHOT_SCHEMA {
        return Err(StateContinuityRefusal::new(
            STATE_CODE_SNAPSHOT_SCHEMA_UNKNOWN,
            format!(
                "saved data declares schema `{}`; this build reads `{INSTANCE_SNAPSHOT_SCHEMA}`",
                snapshot.schema
            ),
        ));
    }
    validate_snapshot(snapshot).map(|_| ()).map_err(|error| {
        StateContinuityRefusal::new(STATE_CODE_SNAPSHOT_INVALID, error.to_string())
    })
}

/// The declared mapping for every state slot of a target Derivation.
pub fn target_state_slots(state: &[BoundState]) -> Vec<TargetStateSlot> {
    state
        .iter()
        .map(|slot| {
            let (save_format, save_version, compatible_versions) = match &slot.save {
                Some(declaration) => (
                    declaration.format.clone(),
                    declaration.version,
                    declaration.compatible_versions.clone(),
                ),
                None => (
                    LEGACY_STATE_SAVE_FORMAT.to_owned(),
                    LEGACY_STATE_SAVE_VERSION,
                    Vec::new(),
                ),
            };
            TargetStateSlot {
                id: slot.id.clone(),
                protocol: slot.protocol.clone(),
                location: if slot.requires_fixed_mount() {
                    StateSlotLocation::Mount {
                        path: slot.mount.clone(),
                    }
                } else {
                    StateSlotLocation::InstancePath {
                        variable: format!(
                            "ATO_STATE_PATH_{}",
                            slot.id.to_ascii_uppercase().replace(['.', '-'], "_")
                        ),
                    }
                },
                save_format,
                save_version,
                compatible_versions,
            }
        })
        .collect()
}

/// The format/version each saved resource declares, or the legacy pair it
/// implies when it declares nothing.
pub fn saved_state_slots(snapshot: &InstanceSnapshotV1) -> Vec<SavedStateSlot> {
    snapshot
        .resources
        .iter()
        .map(|resource| SavedStateSlot {
            slot: resource.slot.clone(),
            protocol: resource.protocol.clone(),
            save_format: resource.save.as_ref().map_or_else(
                || LEGACY_STATE_SAVE_FORMAT.to_owned(),
                |save| save.format.clone(),
            ),
            save_version: resource
                .save
                .as_ref()
                .map_or(LEGACY_STATE_SAVE_VERSION, |save| save.version),
        })
        .collect()
}

fn saved_slot_of(resource: &InstanceSnapshotResourceV1) -> (String, u32) {
    (
        resource.save.as_ref().map_or_else(
            || LEGACY_STATE_SAVE_FORMAT.to_owned(),
            |save| save.format.clone(),
        ),
        resource
            .save
            .as_ref()
            .map_or(LEGACY_STATE_SAVE_VERSION, |save| save.version),
    )
}

/// Compare saved resources with the target Derivation's declared state before a
/// switch. Incompatibility is a named refusal; a version difference the target
/// declares readable is a structured migration that still needs approval.
pub fn compare_state_slots(
    snapshot: &InstanceSnapshotV1,
    target_state: &[BoundState],
) -> Result<StateCompatibility, StateContinuityRefusal> {
    validate_saved_data(snapshot)?;
    let target = target_state
        .iter()
        .map(|slot| (slot.id.as_str(), slot))
        .collect::<BTreeMap<_, _>>();
    let mut migrations = Vec::new();
    for resource in snapshot
        .resources
        .iter()
        .filter(|resource| resource.protocol == STATE_FILESYSTEM_PROTOCOL)
    {
        let Some(slot) = target.get(resource.slot.as_str()) else {
            return Err(StateContinuityRefusal::new(
                STATE_CODE_SLOT_UNDECLARED,
                format!(
                    "saved data contains state slot `{}` that this Derivation does not declare; \
                     switching would silently drop it",
                    resource.slot
                ),
            ));
        };
        if slot.protocol != resource.protocol {
            return Err(StateContinuityRefusal::new(
                STATE_CODE_PROTOCOL_MISMATCH,
                format!(
                    "saved slot `{}` is stored through `{}` but this Derivation declares `{}`",
                    resource.slot, resource.protocol, slot.protocol
                ),
            ));
        }
        let (saved_format, saved_version) = saved_slot_of(resource);
        let target_format = slot
            .save
            .as_ref()
            .map_or(LEGACY_STATE_SAVE_FORMAT, |save| save.format.as_str());
        let target_version = slot
            .save
            .as_ref()
            .map_or(LEGACY_STATE_SAVE_VERSION, |save| save.version);
        if saved_format != target_format {
            return Err(StateContinuityRefusal::new(
                STATE_CODE_FORMAT_INCOMPATIBLE,
                format!(
                    "saved slot `{}` is `{saved_format}` v{saved_version} but this Derivation \
                     saves `{target_format}` v{target_version}; no converter is declared",
                    resource.slot
                ),
            ));
        }
        if saved_version == target_version {
            continue;
        }
        if !slot
            .save
            .as_ref()
            .is_some_and(|save| save.compatible_versions.contains(&saved_version))
        {
            return Err(StateContinuityRefusal::new(
                STATE_CODE_VERSION_INCOMPATIBLE,
                format!(
                    "saved slot `{}` is `{saved_format}` v{saved_version}; this Derivation reads \
                     v{target_version} and does not declare v{saved_version} compatible",
                    resource.slot
                ),
            ));
        }
        migrations.push(StateMigration {
            slot: resource.slot.clone(),
            format: saved_format,
            from_version: saved_version,
            to_version: target_version,
        });
    }
    Ok(if migrations.is_empty() {
        StateCompatibility::Compatible
    } else {
        StateCompatibility::MigrationRequired(migrations)
    })
}

/// The refusal a caller raises when a migration is needed but nobody approved
/// one. Kept here so the reason is worded once.
pub fn migration_approval_refusal(migrations: &[StateMigration]) -> StateContinuityRefusal {
    let plan = migrations
        .iter()
        .map(|migration| {
            format!(
                "{} `{}` v{} -> v{}",
                migration.slot, migration.format, migration.from_version, migration.to_version
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    StateContinuityRefusal::new(
        STATE_CODE_MIGRATION_APPROVAL_REQUIRED,
        format!("restoring this saved data needs an explicit migration approval: {plan}"),
    )
}

/// Transform saved data to the form the target Derivation declares.
///
/// This is the only built-in migration: an identity copy. It is permitted only
/// when the formats are equal and the target declared the saved version
/// readable, so no byte is reinterpreted. The input is never modified.
pub fn migrate_snapshot(
    snapshot: &InstanceSnapshotV1,
    migrations: &[StateMigration],
) -> Result<InstanceSnapshotV1, StateContinuityRefusal> {
    validate_saved_data(snapshot)?;
    let mut migrated = snapshot.clone();
    for migration in migrations {
        let resource = migrated
            .resources
            .iter_mut()
            .find(|resource| resource.slot == migration.slot)
            .ok_or_else(|| {
                StateContinuityRefusal::new(
                    STATE_CODE_SNAPSHOT_INVALID,
                    format!(
                        "saved data changed after the migration plan was computed: slot `{}` is gone",
                        migration.slot
                    ),
                )
            })?;
        let (current_format, current_version) = saved_slot_of(resource);
        if current_format != migration.format || current_version != migration.from_version {
            return Err(StateContinuityRefusal::new(
                STATE_CODE_SNAPSHOT_INVALID,
                format!(
                    "saved data changed after the migration plan was computed: slot `{}` is \
                     `{current_format}` v{current_version}",
                    migration.slot
                ),
            ));
        }
        resource.save = Some(crate::instance_snapshot::InstanceSnapshotSaveV1 {
            format: migration.format.clone(),
            version: migration.to_version,
        });
    }
    validate_saved_data(&migrated)?;
    Ok(migrated)
}

/// Observe what was actually imported: recompute the digest of every saved
/// resource and Asset from the bytes in hand and compare it with the saved-data
/// index. A missing object or a mismatch is a failure, so a reference alone is
/// never evidence of a restore.
pub fn observe_imported_content(
    snapshot: &InstanceSnapshotV1,
    content: &BTreeMap<String, Vec<u8>>,
) -> Result<(), StateContinuityRefusal> {
    for resource in &snapshot.resources {
        let bytes = content.get(&resource.content_ref).ok_or_else(|| {
            StateContinuityRefusal::new(
                STATE_CODE_IMPORT_CONTENT_MISSING,
                format!(
                    "saved slot `{}` is indexed as `{}` but that content was not imported",
                    resource.slot, resource.content_ref
                ),
            )
        })?;
        if bundle_sha256(bytes) != resource.content_ref {
            return Err(StateContinuityRefusal::new(
                STATE_CODE_IMPORT_DIGEST_MISMATCH,
                format!(
                    "imported content for saved slot `{}` does not match its declared digest `{}`",
                    resource.slot, resource.content_ref
                ),
            ));
        }
    }
    for asset in &snapshot.assets {
        let bytes = content.get(&asset.content_ref).ok_or_else(|| {
            StateContinuityRefusal::new(
                STATE_CODE_IMPORT_CONTENT_MISSING,
                format!(
                    "saved Asset `{}` is indexed as `{}` but that content was not imported",
                    asset.alias, asset.content_ref
                ),
            )
        })?;
        if bundle_sha256(bytes) != asset.content_ref || bytes.len() as u64 != asset.size {
            return Err(StateContinuityRefusal::new(
                STATE_CODE_IMPORT_DIGEST_MISMATCH,
                format!(
                    "imported content for saved Asset `{}` does not match its declared digest \
                     `{}`",
                    asset.alias, asset.content_ref
                ),
            ));
        }
    }
    Ok(())
}

/// The save declaration a snapshot resource must carry for a slot whose restored
/// bytes were observed. `None` means the legacy pair.
///
/// Convenience for callers that hold a `PortableApplicationError` and want the
/// saved-data refusal instead of an opaque profile error.
pub fn snapshot_refusal(error: PortableApplicationError) -> StateContinuityRefusal {
    StateContinuityRefusal::new(STATE_CODE_SNAPSHOT_INVALID, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ato_formation::authoring::StateAccess;

    use crate::instance_snapshot::InstanceSnapshotSaveV1;

    fn legacy_snapshot(slot: &str, content: &[u8]) -> InstanceSnapshotV1 {
        InstanceSnapshotV1 {
            schema: INSTANCE_SNAPSHOT_SCHEMA.to_owned(),
            resources: vec![InstanceSnapshotResourceV1 {
                slot: slot.to_owned(),
                protocol: STATE_FILESYSTEM_PROTOCOL.to_owned(),
                content_ref: bundle_sha256(content),
                save: None,
            }],
            assets: Vec::new(),
            asset_bindings: Vec::new(),
        }
    }

    fn declared_snapshot(format: &str, version: u32) -> InstanceSnapshotV1 {
        let mut snapshot = legacy_snapshot("data", b"saved bytes");
        snapshot.resources[0].save = Some(InstanceSnapshotSaveV1 {
            format: format.to_owned(),
            version,
        });
        snapshot
    }

    fn target_slot(
        format: &str,
        version: u32,
        compatible_versions: &[u32],
    ) -> ato_formation::authoring::BoundState {
        ato_formation::authoring::BoundState {
            id: "data".to_owned(),
            protocol: STATE_FILESYSTEM_PROTOCOL.to_owned(),
            mount: "/data".to_owned(),
            access: StateAccess::ReadWrite,
            save: Some(ato_formation::authoring::StateSaveDeclaration {
                format: format.to_owned(),
                version,
                compatible_versions: compatible_versions.to_vec(),
            }),
            mount_required: None,
        }
    }

    #[test]
    fn an_unknown_saved_data_schema_is_a_named_refusal() {
        let mut snapshot = legacy_snapshot("data", b"saved bytes");
        snapshot.schema = "ato.portable-instance-snapshot/2".to_owned();

        let refusal = validate_saved_data(&snapshot).unwrap_err();

        assert_eq!(refusal.code, STATE_CODE_SNAPSHOT_SCHEMA_UNKNOWN);
        assert!(refusal.detail.contains("ato.portable-instance-snapshot/2"));
    }

    #[test]
    fn legacy_declarations_on_both_sides_stay_compatible() {
        let snapshot = legacy_snapshot("data", b"saved bytes");
        let target = ato_formation::authoring::BoundState {
            id: "data".to_owned(),
            protocol: STATE_FILESYSTEM_PROTOCOL.to_owned(),
            mount: "/data".to_owned(),
            access: StateAccess::ReadWrite,
            save: None,
            mount_required: None,
        };

        assert_eq!(
            compare_state_slots(&snapshot, std::slice::from_ref(&target)).unwrap(),
            StateCompatibility::Compatible
        );
    }

    #[test]
    fn a_saved_slot_the_target_does_not_declare_is_refused() {
        let snapshot = legacy_snapshot("data", b"saved bytes");

        let refusal = compare_state_slots(&snapshot, &[]).unwrap_err();

        assert_eq!(refusal.code, STATE_CODE_SLOT_UNDECLARED);
        assert!(refusal.detail.contains("data"));
    }

    #[test]
    fn a_different_declared_format_is_refused_without_a_converter() {
        let snapshot = declared_snapshot("app.notes.save", 1);

        let refusal = compare_state_slots(&snapshot, &[target_slot("app.notes.save.v2", 1, &[])])
            .unwrap_err();

        assert_eq!(refusal.code, STATE_CODE_FORMAT_INCOMPATIBLE);
        assert!(refusal.detail.contains("app.notes.save"));
    }

    #[test]
    fn a_version_difference_without_declared_compatibility_is_refused() {
        let snapshot = declared_snapshot("app.notes.save", 1);

        let refusal =
            compare_state_slots(&snapshot, &[target_slot("app.notes.save", 2, &[])]).unwrap_err();

        assert_eq!(refusal.code, STATE_CODE_VERSION_INCOMPATIBLE);
        assert!(refusal.detail.contains("v1"));
    }

    #[test]
    fn a_declared_compatible_version_is_an_identity_copy_migration() {
        let snapshot = declared_snapshot("app.notes.save", 1);
        let target = target_slot("app.notes.save", 2, &[1]);

        let compatibility = compare_state_slots(&snapshot, std::slice::from_ref(&target)).unwrap();
        let StateCompatibility::MigrationRequired(migrations) = compatibility else {
            panic!("a declared-compatible version must be a migration");
        };
        assert_eq!(
            migrations,
            vec![StateMigration {
                slot: "data".to_owned(),
                format: "app.notes.save".to_owned(),
                from_version: 1,
                to_version: 2,
            }]
        );

        let migrated = migrate_snapshot(&snapshot, &migrations).unwrap();

        assert_eq!(
            migrated.resources[0].save,
            Some(InstanceSnapshotSaveV1 {
                format: "app.notes.save".to_owned(),
                version: 2,
            })
        );
        assert_eq!(
            migrated.resources[0].content_ref, snapshot.resources[0].content_ref,
            "an identity copy does not reinterpret the saved bytes"
        );
        assert_eq!(
            snapshot,
            declared_snapshot("app.notes.save", 1),
            "the original saved data is untouched"
        );
    }

    #[test]
    fn a_stale_migration_plan_is_refused_and_leaves_the_input_alone() {
        let snapshot = declared_snapshot("app.notes.save", 1);
        let stale = [StateMigration {
            slot: "data".to_owned(),
            format: "app.notes.save".to_owned(),
            from_version: 9,
            to_version: 2,
        }];

        let refusal = migrate_snapshot(&snapshot, &stale).unwrap_err();

        assert_eq!(refusal.code, STATE_CODE_SNAPSHOT_INVALID);
        assert_eq!(snapshot, declared_snapshot("app.notes.save", 1));
    }

    #[test]
    fn imported_content_must_match_the_saved_index() {
        let snapshot = legacy_snapshot("data", b"saved bytes");
        let reference = snapshot.resources[0].content_ref.clone();

        let missing = observe_imported_content(&snapshot, &BTreeMap::new()).unwrap_err();
        assert_eq!(missing.code, STATE_CODE_IMPORT_CONTENT_MISSING);

        let wrong = observe_imported_content(
            &snapshot,
            &BTreeMap::from([(reference.clone(), b"other bytes".to_vec())]),
        )
        .unwrap_err();
        assert_eq!(wrong.code, STATE_CODE_IMPORT_DIGEST_MISMATCH);

        observe_imported_content(
            &snapshot,
            &BTreeMap::from([(reference, b"saved bytes".to_vec())]),
        )
        .unwrap();
    }

    #[test]
    fn declarations_stay_absent_from_legacy_canonical_bytes() {
        let snapshot = legacy_snapshot("data", b"saved bytes");
        let bytes = serde_jcs::to_vec(&snapshot).unwrap();
        assert!(
            !String::from_utf8(bytes).unwrap().contains("\"save\""),
            "a legacy snapshot must keep its exact canonical bytes"
        );

        let slot = ato_formation::authoring::BoundState {
            id: "data".to_owned(),
            protocol: STATE_FILESYSTEM_PROTOCOL.to_owned(),
            mount: "/data".to_owned(),
            access: StateAccess::ReadWrite,
            save: None,
            mount_required: None,
        };
        let bytes = serde_jcs::to_vec(&slot).unwrap();
        assert!(
            !String::from_utf8(bytes).unwrap().contains("\"save\""),
            "a legacy Derivation must keep its exact canonical bytes"
        );
    }

    #[test]
    fn the_declared_mapping_names_the_storage_location() {
        let mut slot = target_slot("app.notes.save", 1, &[]);
        let mounted = target_state_slots(std::slice::from_ref(&slot));
        assert_eq!(
            mounted[0].location,
            StateSlotLocation::Mount {
                path: "/data".to_owned()
            }
        );

        slot.mount_required = Some(false);
        let path_delivered = target_state_slots(std::slice::from_ref(&slot));
        assert_eq!(
            path_delivered[0].location,
            StateSlotLocation::InstancePath {
                variable: "ATO_STATE_PATH_DATA".to_owned()
            }
        );
    }
}
