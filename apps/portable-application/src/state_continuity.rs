//! Cross-Derivation Instance state continuity.
//!
//! An Instance's saved data is independent of the Derivation (D) that runs it,
//! but D-independence of the saved *location* is not interoperability of the
//! saved *format*. This module supplies the declared mapping between a state
//! slot and the storage protocol/location it is saved through, the declared
//! save format and version per slot, and the one place that decides whether a
//! Derivation may restore saved data written by another.
//!
//! The rules enforced here, and nowhere else:
//!
//! 1. Saved data declares its schema; a schema this build does not read is a
//!    named refusal, never a partial restore.
//! 2. Every saved resource must be deliverable to the target Derivation: the
//!    target declares the same slot and storage protocol, and the selected
//!    realization can hand that protocol (and the Assets) to the Run.
//! 3. A save format is only comparable when BOTH sides declare it. Nothing here
//!    treats an absent declaration as a shared "legacy format": a D switch with
//!    an undeclared format is refused by name.
//! 4. A version difference is refused unless the target declares the saved
//!    version readable. Even then the only migration is an identity copy: the
//!    saved data keeps its bytes AND its declared version, because reading a
//!    version is not converting it. v0 has no byte converter.
//! 5. Restore evidence is the digest recomputed from the bytes actually
//!    imported, not the presence of a snapshot reference.
//!
//! Nothing here knows about Instance ids, storage paths or processes: those are
//! local orchestration and never enter `ContractRef`/`DerivationRef`.

use std::collections::BTreeMap;
use std::fmt;

use ato_formation::authoring::{BoundState, STATE_FILESYSTEM_PROTOCOL};

use crate::PortableRealizationKind;
use crate::bundle_sha256;
use crate::instance_snapshot::{
    BROWSER_INSTANCE_STATE_PROTOCOL, INSTANCE_SNAPSHOT_SCHEMA, InstanceSnapshotV1,
    validate_snapshot,
};

/// The declared mapping for one state slot: which storage protocol and location
/// a saved resource must name, and which save format the slot declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetStateSlot {
    pub id: String,
    pub protocol: String,
    /// Where the route expects the slot: a fixed mount, or the host path handed
    /// to the workload through `ATO_STATE_PATH_<ID>`.
    pub location: StateSlotLocation,
    /// The save format this route declares. `None` means it declares none, which
    /// is not the same as declaring a common format.
    pub save_format: Option<String>,
    pub save_version: Option<u32>,
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
    pub save_format: Option<String>,
    pub save_version: Option<u32>,
}

/// A migration the target Derivation declared it can read unchanged.
///
/// This is an identity copy: the bytes and the saved data's declared version
/// stay as they are. `data_version` is what the saved data IS; `target_writes`
/// is what the target Derivation writes. They are deliberately different fields
/// so a reader is never reported as a converter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateMigration {
    pub slot: String,
    pub format: String,
    /// The version the saved resource declares. Unchanged by the migration.
    pub data_version: u32,
    /// The version the target Derivation writes. It declared `data_version`
    /// readable; nothing here rewrites the saved data to this version.
    pub target_writes: u32,
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
pub const STATE_CODE_RESOURCE_NOT_DELIVERABLE: &str = "state-resource-not-deliverable";
pub const STATE_CODE_FORMAT_UNDECLARED: &str = "state-save-format-undeclared";
pub const STATE_CODE_FORMAT_INCOMPATIBLE: &str = "state-save-format-incompatible";
pub const STATE_CODE_VERSION_INCOMPATIBLE: &str = "state-save-version-incompatible";
pub const STATE_CODE_MIGRATION_APPROVAL_REQUIRED: &str = "state-migration-approval-required";
pub const STATE_CODE_IMPORT_CONTENT_MISSING: &str = "state-import-content-missing";
pub const STATE_CODE_IMPORT_DIGEST_MISMATCH: &str = "state-import-digest-mismatch";
pub const STATE_CODE_RESTORE_DIGEST_MISMATCH: &str = "state-restore-digest-mismatch";
/// The Run gate: the state the Run would receive is not the saved state.
pub const STATE_CODE_INSTANCE_STATE_UNVERIFIED: &str = "instance_state_unverified";

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
                    Some(declaration.format.clone()),
                    Some(declaration.version),
                    declaration.compatible_versions.clone(),
                ),
                None => (None, None, Vec::new()),
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

/// The format/version each saved resource declares, if it declares one.
pub fn saved_state_slots(snapshot: &InstanceSnapshotV1) -> Vec<SavedStateSlot> {
    snapshot
        .resources
        .iter()
        .map(|resource| SavedStateSlot {
            slot: resource.slot.clone(),
            protocol: resource.protocol.clone(),
            save_format: resource.save.as_ref().map(|save| save.format.clone()),
            save_version: resource.save.as_ref().map(|save| save.version),
        })
        .collect()
}

/// Whether the selected realization can hand this saved data to the Run.
///
/// A resource whose protocol is not a state slot the Derivation declares is not
/// delivered by magic: only the static surface receives browser state and
/// Assets, a dynamic route receives its declared filesystem slots, and a
/// `ato.data.json@1` resource is not delivered to any v0 route. Anything
/// undeliverable is refused by name instead of being restored and ignored.
pub fn check_state_delivery(
    snapshot: &InstanceSnapshotV1,
    target_state: &[BoundState],
    realization: PortableRealizationKind,
) -> Result<(), StateContinuityRefusal> {
    let target = target_state
        .iter()
        .map(|slot| (slot.id.as_str(), slot))
        .collect::<BTreeMap<_, _>>();
    let mut browser_resources = 0usize;
    for resource in &snapshot.resources {
        match resource.protocol.as_str() {
            STATE_FILESYSTEM_PROTOCOL => {
                let Some(slot) = target.get(resource.slot.as_str()) else {
                    return Err(StateContinuityRefusal::new(
                        STATE_CODE_SLOT_UNDECLARED,
                        format!(
                            "saved data contains state slot `{}` that this Derivation does not \
                             declare; switching would silently drop it",
                            resource.slot
                        ),
                    ));
                };
                if slot.protocol != resource.protocol {
                    return Err(StateContinuityRefusal::new(
                        STATE_CODE_PROTOCOL_MISMATCH,
                        format!(
                            "saved slot `{}` is stored through `{}` but this Derivation declares \
                             `{}`",
                            resource.slot, resource.protocol, slot.protocol
                        ),
                    ));
                }
            }
            BROWSER_INSTANCE_STATE_PROTOCOL => {
                browser_resources += 1;
                if realization != PortableRealizationKind::StaticWeb {
                    return Err(StateContinuityRefusal::new(
                        STATE_CODE_RESOURCE_NOT_DELIVERABLE,
                        format!(
                            "saved slot `{}` holds browser state, which only a static web \
                             surface receives; the selected Derivation realizes {:?}",
                            resource.slot, realization
                        ),
                    ));
                }
            }
            protocol => {
                return Err(StateContinuityRefusal::new(
                    STATE_CODE_RESOURCE_NOT_DELIVERABLE,
                    format!(
                        "saved slot `{}` uses protocol `{protocol}`, which this runtime does not \
                         deliver to the selected Derivation",
                        resource.slot
                    ),
                ));
            }
        }
    }
    if browser_resources > 1 {
        return Err(StateContinuityRefusal::new(
            STATE_CODE_RESOURCE_NOT_DELIVERABLE,
            "the local surface delivers at most one browser-state resource".to_owned(),
        ));
    }
    if !snapshot.assets.is_empty() && realization != PortableRealizationKind::StaticWeb {
        return Err(StateContinuityRefusal::new(
            STATE_CODE_RESOURCE_NOT_DELIVERABLE,
            format!(
                "saved data carries {} Asset(s), which only a static web surface receives; the \
                 selected Derivation realizes {:?}",
                snapshot.assets.len(),
                realization
            ),
        ));
    }
    Ok(())
}

/// The filesystem-slot comparison: delivery is checked by the caller.
fn compare_filesystem_slots(
    snapshot: &InstanceSnapshotV1,
    target_state: &[BoundState],
) -> Result<StateCompatibility, StateContinuityRefusal> {
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
        // A missing declaration is not a shared format. Both sides must state
        // what the bytes are before a D switch may restore them.
        let Some(saved) = resource.save.as_ref() else {
            return Err(StateContinuityRefusal::new(
                STATE_CODE_FORMAT_UNDECLARED,
                format!(
                    "saved slot `{}` does not declare its save format; a Derivation switch \
                     cannot assume one",
                    resource.slot
                ),
            ));
        };
        let Some(declared) = slot.save.as_ref() else {
            return Err(StateContinuityRefusal::new(
                STATE_CODE_FORMAT_UNDECLARED,
                format!(
                    "this Derivation does not declare a save format for slot `{}`; saved data \
                     cannot be assumed readable",
                    resource.slot
                ),
            ));
        };
        if saved.format != declared.format {
            return Err(StateContinuityRefusal::new(
                STATE_CODE_FORMAT_INCOMPATIBLE,
                format!(
                    "saved slot `{}` is `{}` v{} but this Derivation saves `{}` v{}; no \
                     converter is declared",
                    resource.slot, saved.format, saved.version, declared.format, declared.version
                ),
            ));
        }
        if saved.version == declared.version {
            continue;
        }
        if !declared.compatible_versions.contains(&saved.version) {
            return Err(StateContinuityRefusal::new(
                STATE_CODE_VERSION_INCOMPATIBLE,
                format!(
                    "saved slot `{}` holds `{}` data v{}; this Derivation writes v{} and does \
                     not declare v{} readable",
                    resource.slot, saved.format, saved.version, declared.version, saved.version
                ),
            ));
        }
        migrations.push(StateMigration {
            slot: resource.slot.clone(),
            format: saved.format.clone(),
            data_version: saved.version,
            target_writes: declared.version,
        });
    }
    Ok(if migrations.is_empty() {
        StateCompatibility::Compatible
    } else {
        StateCompatibility::MigrationRequired(migrations)
    })
}

/// Compare saved resources with the target Derivation before a D switch.
///
/// Incompatibility is a named refusal; a version difference the target declares
/// readable is a structured migration that still needs explicit approval.
pub fn compare_state_slots(
    snapshot: &InstanceSnapshotV1,
    target_state: &[BoundState],
    realization: PortableRealizationKind,
) -> Result<StateCompatibility, StateContinuityRefusal> {
    validate_saved_data(snapshot)?;
    check_state_delivery(snapshot, target_state, realization)?;
    compare_filesystem_slots(snapshot, target_state)
}

/// The same comparison, minus delivery, for the moment before the bundle is
/// validated.
///
/// Bundles are validated after the import has already decided whether the
/// selected Derivation can read the saved data; running the slot/protocol/
/// format comparison first is what makes that refusal name the state reason
/// instead of a generic profile violation. Delivery of non-filesystem protocols
/// needs the validated realization, so it is checked by [`compare_state_slots`]
/// afterwards.
pub fn compare_state_slots_before_admission(
    snapshot: &InstanceSnapshotV1,
    target_state: &[BoundState],
) -> Result<StateCompatibility, StateContinuityRefusal> {
    validate_saved_data(snapshot)?;
    compare_filesystem_slots(snapshot, target_state)
}

/// The refusal a caller raises when a migration is needed but nobody approved
/// one. Kept here so the reason is worded once.
pub fn migration_approval_refusal(migrations: &[StateMigration]) -> StateContinuityRefusal {
    let plan = migrations
        .iter()
        .map(|migration| {
            format!(
                "{} `{}` data v{} (target writes v{})",
                migration.slot, migration.format, migration.data_version, migration.target_writes
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    StateContinuityRefusal::new(
        STATE_CODE_MIGRATION_APPROVAL_REQUIRED,
        format!("restoring this saved data needs an explicit migration approval: {plan}"),
    )
}

/// Verify — and perform — the only migration v0 has: an identity copy.
///
/// The saved data keeps its bytes AND its declared version. The target declared
/// the saved version readable; that is a read capability, not a conversion, so
/// nothing here rewrites the resource's declaration. The returned snapshot is
/// the saved data unchanged; the caller's approval is what changes.
pub fn migrate_snapshot(
    snapshot: &InstanceSnapshotV1,
    migrations: &[StateMigration],
) -> Result<InstanceSnapshotV1, StateContinuityRefusal> {
    validate_saved_data(snapshot)?;
    for migration in migrations {
        let resource = snapshot
            .resources
            .iter()
            .find(|resource| resource.slot == migration.slot)
            .ok_or_else(|| {
                StateContinuityRefusal::new(
                    STATE_CODE_SNAPSHOT_INVALID,
                    format!(
                        "saved data changed after the migration plan was computed: slot `{}` is \
                         gone",
                        migration.slot
                    ),
                )
            })?;
        let save = resource.save.as_ref().ok_or_else(|| {
            StateContinuityRefusal::new(
                STATE_CODE_FORMAT_UNDECLARED,
                format!(
                    "saved data changed after the migration plan was computed: slot `{}` no \
                     longer declares its format",
                    migration.slot
                ),
            )
        })?;
        if save.format != migration.format || save.version != migration.data_version {
            return Err(StateContinuityRefusal::new(
                STATE_CODE_SNAPSHOT_INVALID,
                format!(
                    "saved data changed after the migration plan was computed: slot `{}` is \
                     `{}` v{}",
                    migration.slot, save.format, save.version
                ),
            ));
        }
    }
    Ok(snapshot.clone())
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

/// The Run gate: the live state the Run will actually receive must observe the
/// saved digests. `detail` names which observation failed.
pub fn run_state_unverified(detail: impl fmt::Display) -> StateContinuityRefusal {
    StateContinuityRefusal::new(STATE_CODE_INSTANCE_STATE_UNVERIFIED, detail.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ato_formation::authoring::StateAccess;

    use crate::instance_snapshot::{
        DATA_JSON_PROTOCOL, InstanceSnapshotResourceV1, InstanceSnapshotSaveV1,
    };

    fn snapshot_with(
        slot: &str,
        protocol: &str,
        content: &[u8],
        save: Option<(&str, u32)>,
    ) -> InstanceSnapshotV1 {
        InstanceSnapshotV1 {
            schema: INSTANCE_SNAPSHOT_SCHEMA.to_owned(),
            resources: vec![InstanceSnapshotResourceV1 {
                slot: slot.to_owned(),
                protocol: protocol.to_owned(),
                content_ref: bundle_sha256(content),
                save: save.map(|(format, version)| InstanceSnapshotSaveV1 {
                    format: format.to_owned(),
                    version,
                }),
            }],
            assets: Vec::new(),
            asset_bindings: Vec::new(),
        }
    }

    fn legacy_snapshot(slot: &str, content: &[u8]) -> InstanceSnapshotV1 {
        snapshot_with(slot, STATE_FILESYSTEM_PROTOCOL, content, None)
    }

    fn declared_snapshot(format: &str, version: u32) -> InstanceSnapshotV1 {
        snapshot_with(
            "data",
            STATE_FILESYSTEM_PROTOCOL,
            b"saved bytes",
            Some((format, version)),
        )
    }

    fn declared_slot(
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

    const PROCESS: PortableRealizationKind = PortableRealizationKind::LocalProcess;
    const STATIC: PortableRealizationKind = PortableRealizationKind::StaticWeb;

    #[test]
    fn an_unknown_saved_data_schema_is_a_named_refusal() {
        let mut snapshot = legacy_snapshot("data", b"saved bytes");
        snapshot.schema = "ato.portable-instance-snapshot/2".to_owned();

        let refusal = validate_saved_data(&snapshot).unwrap_err();

        assert_eq!(refusal.code, STATE_CODE_SNAPSHOT_SCHEMA_UNKNOWN);
        assert!(refusal.detail.contains("ato.portable-instance-snapshot/2"));
    }

    #[test]
    fn an_undeclared_save_format_is_refused_on_a_switch() {
        // Both sides undeclared: no shared "legacy format" is assumed.
        let snapshot = legacy_snapshot("data", b"saved bytes");
        let target = declared_slot("app.notes.save", 1, &[]);
        assert_eq!(
            compare_state_slots(&snapshot, std::slice::from_ref(&target), PROCESS)
                .unwrap_err()
                .code,
            STATE_CODE_FORMAT_UNDECLARED
        );

        // Only the target undeclared: equally unproven.
        let declared = declared_snapshot("app.notes.save", 1);
        let mut undeclared_target = declared_slot("app.notes.save", 1, &[]);
        undeclared_target.save = None;
        assert_eq!(
            compare_state_slots(&declared, std::slice::from_ref(&undeclared_target), PROCESS)
                .unwrap_err()
                .code,
            STATE_CODE_FORMAT_UNDECLARED
        );
    }

    #[test]
    fn a_saved_slot_the_target_does_not_declare_is_refused() {
        let snapshot = declared_snapshot("app.notes.save", 1);

        let refusal = compare_state_slots(&snapshot, &[], PROCESS).unwrap_err();

        assert_eq!(refusal.code, STATE_CODE_SLOT_UNDECLARED);
        assert!(refusal.detail.contains("data"));
    }

    #[test]
    fn a_different_declared_format_is_refused_without_a_converter() {
        let snapshot = declared_snapshot("app.notes.save", 1);

        let refusal = compare_state_slots(
            &snapshot,
            &[declared_slot("app.notes.other-save", 1, &[])],
            PROCESS,
        )
        .unwrap_err();

        assert_eq!(refusal.code, STATE_CODE_FORMAT_INCOMPATIBLE);
        assert!(refusal.detail.contains("app.notes.save"));
    }

    #[test]
    fn a_version_difference_without_declared_compatibility_is_refused() {
        let snapshot = declared_snapshot("app.notes.save", 1);

        let refusal = compare_state_slots(
            &snapshot,
            &[declared_slot("app.notes.save", 2, &[])],
            PROCESS,
        )
        .unwrap_err();

        assert_eq!(refusal.code, STATE_CODE_VERSION_INCOMPATIBLE);
        assert!(refusal.detail.contains("data v1"));
    }

    #[test]
    fn a_declared_compatible_version_is_an_identity_copy_migration() {
        let snapshot = declared_snapshot("app.notes.save", 1);
        let target = declared_slot("app.notes.save", 2, &[1]);

        let compatibility =
            compare_state_slots(&snapshot, std::slice::from_ref(&target), PROCESS).unwrap();
        let StateCompatibility::MigrationRequired(migrations) = compatibility else {
            panic!("a declared-compatible version must be a migration");
        };
        assert_eq!(
            migrations,
            vec![StateMigration {
                slot: "data".to_owned(),
                format: "app.notes.save".to_owned(),
                data_version: 1,
                target_writes: 2,
            }]
        );

        let migrated = migrate_snapshot(&snapshot, &migrations).unwrap();

        // Reading v1 is not converting it: the declaration and the bytes stay.
        assert_eq!(
            migrated.resources[0].save,
            Some(InstanceSnapshotSaveV1 {
                format: "app.notes.save".to_owned(),
                version: 1,
            }),
            "an identity-copy migration must not rewrite the saved version"
        );
        assert_eq!(migrated, snapshot, "the saved data is unchanged");
    }

    #[test]
    fn a_stale_migration_plan_is_refused_and_leaves_the_input_alone() {
        let snapshot = declared_snapshot("app.notes.save", 1);
        let stale = [StateMigration {
            slot: "data".to_owned(),
            format: "app.notes.save".to_owned(),
            data_version: 9,
            target_writes: 2,
        }];

        let refusal = migrate_snapshot(&snapshot, &stale).unwrap_err();

        assert_eq!(refusal.code, STATE_CODE_SNAPSHOT_INVALID);
        assert_eq!(snapshot, declared_snapshot("app.notes.save", 1));
    }

    #[test]
    fn a_non_filesystem_resource_must_be_deliverable() {
        // Browser state reaches the static surface only.
        let browser = snapshot_with("main", BROWSER_INSTANCE_STATE_PROTOCOL, b"{}", None);
        assert_eq!(
            check_state_delivery(&browser, &[], PROCESS)
                .unwrap_err()
                .code,
            STATE_CODE_RESOURCE_NOT_DELIVERABLE
        );
        check_state_delivery(&browser, &[], STATIC).unwrap();

        // A data.json resource is delivered to no v0 route.
        let data = snapshot_with("main", DATA_JSON_PROTOCOL, b"{}", None);
        assert_eq!(
            check_state_delivery(&data, &[], STATIC).unwrap_err().code,
            STATE_CODE_RESOURCE_NOT_DELIVERABLE
        );

        // Assets travel with the static surface only.
        let mut with_asset = snapshot_with("main", BROWSER_INSTANCE_STATE_PROTOCOL, b"{}", None);
        with_asset
            .assets
            .push(crate::instance_snapshot::InstanceSnapshotAssetV1 {
                alias: "asset-1".to_owned(),
                content_ref: bundle_sha256(b"asset"),
                filename: "photo.jpg".to_owned(),
                content_type: "image/jpeg".to_owned(),
                size: 5,
            });
        assert_eq!(
            check_state_delivery(&with_asset, &[], PROCESS)
                .unwrap_err()
                .code,
            STATE_CODE_RESOURCE_NOT_DELIVERABLE
        );
        check_state_delivery(&with_asset, &[], STATIC).unwrap();
    }

    #[test]
    fn a_browser_snapshot_compares_compatible_for_a_static_surface() {
        let browser = snapshot_with("main", BROWSER_INSTANCE_STATE_PROTOCOL, b"{}", None);
        assert_eq!(
            compare_state_slots(&browser, &[], STATIC).unwrap(),
            StateCompatibility::Compatible
        );
    }

    #[test]
    fn declared_formats_that_match_with_the_same_version_are_compatible() {
        let snapshot = declared_snapshot("app.notes.save", 2);
        assert_eq!(
            compare_state_slots(
                &snapshot,
                &[declared_slot("app.notes.save", 2, &[])],
                PROCESS
            )
            .unwrap(),
            StateCompatibility::Compatible
        );
    }

    #[test]
    fn imported_content_must_match_the_saved_index() {
        let snapshot = declared_snapshot("app.notes.save", 1);
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
        let mut slot = declared_slot("app.notes.save", 1, &[]);
        let mounted = target_state_slots(std::slice::from_ref(&slot));
        assert_eq!(
            mounted[0].location,
            StateSlotLocation::Mount {
                path: "/data".to_owned()
            }
        );
        assert_eq!(mounted[0].save_format.as_deref(), Some("app.notes.save"));
        assert_eq!(mounted[0].save_version, Some(1));

        slot.mount_required = Some(false);
        let path_delivered = target_state_slots(std::slice::from_ref(&slot));
        assert_eq!(
            path_delivered[0].location,
            StateSlotLocation::InstancePath {
                variable: "ATO_STATE_PATH_DATA".to_owned()
            }
        );

        let undeclared = ato_formation::authoring::BoundState {
            id: "data".to_owned(),
            protocol: STATE_FILESYSTEM_PROTOCOL.to_owned(),
            mount: "/data".to_owned(),
            access: StateAccess::ReadWrite,
            save: None,
            mount_required: None,
        };
        let mapping = target_state_slots(std::slice::from_ref(&undeclared));
        assert_eq!(mapping[0].save_format, None, "undeclared stays undeclared");
        assert_eq!(mapping[0].save_version, None);
    }
}
