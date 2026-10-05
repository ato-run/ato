//! Explicit, Runtime-private state resolution for functional verification.
//! The authenticated caller owns assignment, writer lifecycle and working copies.
//! None of these types serializes a path or grants authority from a descriptor.

use std::{collections::BTreeSet, path::Path};

use anyhow::{Context, Result, ensure};
use ato_formation::authoring::{BoundState, StateAccess};
use ato_ipc::runtime_launch::{LaunchContextV1, StateAccessV1, StateAttachmentV1};

use crate::launch::resolved::ResolvedStateAttachment;

/// An explicit mapping from a declared D slot to an already assigned attachment.
/// Construct only after authenticating its owner, Run/lease and current fence.
pub struct VerificationStateAttachment<'a> {
    pub slot_id: &'a str,
    pub declaration: &'a StateAttachmentV1,
    pub resolved: &'a ResolvedStateAttachment,
}

/// Borrowed state of an explicitly authorized, isolated verification instance.
/// The caller retains the writer/working-copy ownership until confirmed stop,
/// then commits or releases using the existing control-plane state lifecycle.
pub struct VerificationStateBindings<'a> {
    pub context: &'a LaunchContextV1,
    pub attachments: &'a [VerificationStateAttachment<'a>],
}

impl VerificationStateBindings<'_> {
    pub(crate) fn validate(
        &self,
        slots: &[BoundState],
        workspace: &Path,
        scratch: &Path,
    ) -> Result<()> {
        ensure!(
            !self.attachments.is_empty() && self.attachments.len() == slots.len(),
            "verification_state_binding_incomplete"
        );
        ensure!(
            [
                &self.context.run_id,
                &self.context.compute_id,
                &self.context.compute_schema_id,
                &self.context.compute_instance_id
            ]
            .iter()
            .all(|id| !id.is_empty() && !id.contains('\0')),
            "verification_state_assignment_invalid"
        );
        let workspace = workspace
            .canonicalize()
            .context("verification workspace unavailable")?;
        let scratch = resolved_cleanup_root(scratch)?;
        let mut ids = BTreeSet::new();
        let mut keys = BTreeSet::new();
        let mut mounts = BTreeSet::new();
        let mut paths: Vec<std::path::PathBuf> = Vec::new();
        for binding in self.attachments {
            let logical = binding.declaration;
            let physical = binding.resolved;
            ensure!(
                ids.insert(binding.slot_id)
                    && keys.insert(logical.state_key.as_str())
                    && mounts.insert(logical.mount_target.as_str()),
                "verification_state_binding_duplicate"
            );
            let slot = slots
                .iter()
                .find(|s| s.id == binding.slot_id)
                .context("verification_state_slot_undeclared")?;
            let access = match slot.access {
                StateAccess::ReadOnly => StateAccessV1::ReadOnly,
                StateAccess::ReadWrite => StateAccessV1::ReadWrite,
            };
            ensure!(
                ato_formation::proposal::isolated_state_id(&slot.id)
                    && ato_formation::proposal::isolated_state_mount(&slot.mount)
                    && slot.protocol == ato_formation::authoring::STATE_FILESYSTEM_PROTOCOL
                    && logical.mount_target == slot.mount
                    && logical.access == access,
                "verification_state_requirement_mismatch"
            );
            ensure!(
                !logical.state_key.is_empty()
                    && logical.state_key == physical.state_key()
                    && logical.revision_ref.as_deref() == physical.revision_ref()
                    && logical.mount_target == physical.guest_target()
                    && logical.access == physical.access(),
                "verification_state_resolution_mismatch"
            );
            ensure!(
                match access {
                    StateAccessV1::ReadWrite => logical.writer_fence.is_some_and(|f| f > 0),
                    StateAccessV1::ReadOnly => logical.writer_fence.is_none(),
                },
                "verification_state_writer_fence_required"
            );
            let path = physical.working_copy_for_mount();
            let canonical = path
                .canonicalize()
                .context("verification state unavailable")?;
            ensure!(
                path.is_absolute()
                    && path == canonical
                    && std::fs::symlink_metadata(path)?.is_dir(),
                "verification_state_path_invalid"
            );
            ensure!(
                !overlap(&canonical, &workspace)
                    && !overlap(&canonical, &scratch)
                    && paths.iter().all(|other| !overlap(&canonical, other)),
                "verification_state_cleanup_overlap"
            );
            paths.push(canonical);
        }
        Ok(())
    }

    pub(crate) fn project(&self) -> (Vec<StateAttachmentV1>, Vec<ResolvedStateAttachment>) {
        let logical = self
            .attachments
            .iter()
            .map(|b| b.declaration.clone())
            .collect();
        let physical = self
            .attachments
            .iter()
            .map(|b| {
                let s = b.resolved;
                ResolvedStateAttachment::new(
                    s.state_key(),
                    s.revision_ref().map(str::to_owned),
                    s.working_copy_for_mount().to_owned(),
                    s.guest_target(),
                    s.access(),
                )
            })
            .collect();
        (logical, physical)
    }
}

fn overlap(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// Resolve existing ancestors without requiring the candidate scratch to exist.
fn resolved_cleanup_root(path: &Path) -> Result<std::path::PathBuf> {
    let path = std::path::absolute(path)?;
    ensure!(
        !path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir)),
        "verification_state_cleanup_path_invalid"
    );
    let mut ancestor = path.as_path();
    let mut suffix = Vec::new();
    while !ancestor.exists() {
        suffix.push(
            ancestor
                .file_name()
                .context("verification cleanup root unavailable")?,
        );
        ancestor = ancestor
            .parent()
            .context("verification cleanup root unavailable")?;
    }
    let mut resolved = ancestor.canonicalize()?;
    for part in suffix.into_iter().rev() {
        resolved.push(part);
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf};

    struct Fixture {
        _root: tempfile::TempDir,
        workspace: PathBuf,
        scratch: PathBuf,
        state: PathBuf,
        context: LaunchContextV1,
        declaration: StateAttachmentV1,
        slot: BoundState,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().expect("fixture root");
            let canonical = root.path().canonicalize().expect("canonical root");
            let workspace = canonical.join("source");
            let state = canonical.join("assigned-state");
            fs::create_dir(&workspace).expect("workspace");
            fs::create_dir(&state).expect("state");
            Self {
                workspace,
                scratch: canonical.join("attempt/realization"),
                state,
                _root: root,
                context: LaunchContextV1 {
                    run_id: "assigned-verification-run".into(),
                    compute_id: "owned-compute".into(),
                    compute_schema_id: "owned-schema".into(),
                    compute_instance_id: "owned-verification-instance".into(),
                },
                declaration: StateAttachmentV1 {
                    state_key: "app_data".into(),
                    revision_ref: Some("revision-existing".into()),
                    mount_target: "/data".into(),
                    access: StateAccessV1::ReadWrite,
                    writer_fence: Some(7),
                },
                slot: BoundState {
                    id: "app.data".into(),
                    protocol: ato_formation::authoring::STATE_FILESYSTEM_PROTOCOL.into(),
                    mount: "/data".into(),
                    access: StateAccess::ReadWrite,
                },
            }
        }

        fn resolved(&self) -> ResolvedStateAttachment {
            ResolvedStateAttachment::new(
                &self.declaration.state_key,
                self.declaration.revision_ref.clone(),
                self.state.clone(),
                &self.declaration.mount_target,
                self.declaration.access,
            )
        }

        fn validate(&self, resolved: &ResolvedStateAttachment) -> Result<()> {
            VerificationStateBindings {
                context: &self.context,
                attachments: &[VerificationStateAttachment {
                    slot_id: &self.slot.id,
                    declaration: &self.declaration,
                    resolved,
                }],
            }
            .validate(
                std::slice::from_ref(&self.slot),
                &self.workspace,
                &self.scratch,
            )
        }
    }

    #[test]
    fn assigned_mapping_preserves_revision_fence_and_private_resolution() {
        let f = Fixture::new();
        let resolved = f.resolved();
        f.validate(&resolved)
            .expect("different logical slot and assigned key allowed");
        let bindings = VerificationStateBindings {
            context: &f.context,
            attachments: &[VerificationStateAttachment {
                slot_id: &f.slot.id,
                declaration: &f.declaration,
                resolved: &resolved,
            }],
        };
        let (logical, physical) = bindings.project();
        assert_eq!(logical, vec![f.declaration.clone()]);
        assert_eq!(physical[0].working_copy_for_mount(), f.state);
        assert!(!format!("{:?}", physical[0]).contains(f.state.to_str().expect("UTF8")));
    }

    #[test]
    fn mismatched_private_revision_is_refused() {
        let f = Fixture::new();
        let wrong = ResolvedStateAttachment::new(
            "app_data",
            None,
            f.state.clone(),
            "/data",
            StateAccessV1::ReadWrite,
        );
        assert!(
            f.validate(&wrong)
                .unwrap_err()
                .to_string()
                .contains("resolution_mismatch")
        );
    }

    #[test]
    fn writable_state_requires_assigned_positive_fence() {
        let mut f = Fixture::new();
        for fence in [None, Some(0)] {
            f.declaration.writer_fence = fence;
            assert!(
                f.validate(&f.resolved())
                    .unwrap_err()
                    .to_string()
                    .contains("writer_fence")
            );
        }
    }

    #[test]
    fn read_only_state_cannot_redeem_writer_generation() {
        let mut f = Fixture::new();
        f.slot.access = StateAccess::ReadOnly;
        f.declaration.access = StateAccessV1::ReadOnly;
        assert!(
            f.validate(&f.resolved())
                .unwrap_err()
                .to_string()
                .contains("writer_fence")
        );
        f.declaration.writer_fence = None;
        f.validate(&f.resolved())
            .expect("assigned reader has no fence");
    }

    #[test]
    fn missing_or_duplicate_slots_are_refused() {
        let f = Fixture::new();
        let resolved = f.resolved();
        let missing = VerificationStateBindings {
            context: &f.context,
            attachments: &[],
        };
        assert!(
            missing
                .validate(std::slice::from_ref(&f.slot), &f.workspace, &f.scratch)
                .is_err()
        );
        let entries = [
            VerificationStateAttachment {
                slot_id: &f.slot.id,
                declaration: &f.declaration,
                resolved: &resolved,
            },
            VerificationStateAttachment {
                slot_id: &f.slot.id,
                declaration: &f.declaration,
                resolved: &resolved,
            },
        ];
        let mut other = f.slot.clone();
        other.id = "app.cache".into();
        let duplicate = VerificationStateBindings {
            context: &f.context,
            attachments: &entries,
        };
        assert!(
            duplicate
                .validate(&[f.slot.clone(), other], &f.workspace, &f.scratch)
                .unwrap_err()
                .to_string()
                .contains("duplicate")
        );
    }

    #[test]
    fn undeclared_slot_or_changed_guest_access_is_refused() {
        let mut f = Fixture::new();
        let resolved = f.resolved();
        f.slot.mount = "/state".into();
        assert!(
            f.validate(&resolved)
                .unwrap_err()
                .to_string()
                .contains("requirement_mismatch")
        );
        f.slot.mount = "/data".into();
        f.slot.access = StateAccess::ReadOnly;
        assert!(
            f.validate(&resolved)
                .unwrap_err()
                .to_string()
                .contains("requirement_mismatch")
        );
        f.slot.access = StateAccess::ReadWrite;
        let undeclared = VerificationStateBindings {
            context: &f.context,
            attachments: &[VerificationStateAttachment {
                slot_id: "app.unknown",
                declaration: &f.declaration,
                resolved: &resolved,
            }],
        };
        assert!(
            undeclared
                .validate(std::slice::from_ref(&f.slot), &f.workspace, &f.scratch)
                .unwrap_err()
                .to_string()
                .contains("undeclared")
        );
    }

    #[test]
    fn state_inside_workspace_or_cleanup_tree_is_refused_without_deletion() {
        let mut f = Fixture::new();
        for path in [f.workspace.join("state"), f.scratch.join("state")] {
            fs::create_dir_all(&path).expect("owned overlapping directory");
            fs::write(path.join("keep"), "preserve").expect("sentinel");
            f.state = path;
            assert!(
                f.validate(&f.resolved())
                    .unwrap_err()
                    .to_string()
                    .contains("cleanup_overlap")
            );
            assert_eq!(
                fs::read_to_string(f.state.join("keep")).expect("not deleted"),
                "preserve"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_working_copy_is_refused() {
        let mut f = Fixture::new();
        let alias = f.state.with_file_name("alias");
        std::os::unix::fs::symlink(&f.state, &alias).expect("symlink fixture");
        f.state = alias;
        assert!(
            f.validate(&f.resolved())
                .unwrap_err()
                .to_string()
                .contains("path_invalid")
        );
    }
}
