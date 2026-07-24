//! `ProgramSourceProjectionV1` — the pinned program source projection of a
//! Capsule declaration (ADR-014 Decision §1).
//!
//! The projection is a pure function of (tree, selected root): the control
//! files — `<root>/capsule.toml` plus the ONE selected canonical lock path —
//! are resolved by exact path first and then excluded; **every other path is
//! ordinary source and is hashed, regardless of its file name or content**.
//! There is no "manifest-shaped TOML" predicate and no content sniffing: a
//! nested `fixtures/capsule.lock` or `examples/capsule.toml` is test-data
//! bytes and changes the digest like any other source file.
//!
//! Normative order (§1, r9):
//!
//! 1. A1v2 admissibility over the ORIGINAL tree, in full — including the
//!    control files. A control file that is a symlink, FIFO, or device fails
//!    closed here; exclusion never hides it from admissibility.
//! 2. Verify `<selected-root>/capsule.toml` exists as a regular file.
//! 3. Resolve [`CapsuleControlFiles`]; coexistence of `capsule.lock` and
//!    `ato.lock.json` at the selected root rejects here (split-brain — never
//!    exclude both, never choose silently).
//! 4. Exclude exactly the resolved control-file paths. Nothing else.
//! 5. Materialize the projected tree preserving bytes AND the executable bit
//!    (A1 file identity includes the executable bit).
//! 6. [`materialized_source_tree_hash`] over the projected root — the
//!    existing, frozen A1 digest, called and never modified.
//!
//! Self-reference invariant: the digest is identical across {no lock,
//! `capsule.lock`, `ato.lock.json`} at the selected root — the canonical lock
//! never reaches the preimage, so `capsule_program_id` is stable across the
//! lock-file rename migration and across lock rewrites that embed
//! `program_identity`.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use crate::capsule_program_contract::{
    CapsuleProgramError, ProgramSourceContract, ProgramSourceDigest,
    ProgramSourceProjectionSchemaV1,
};
use crate::foundation::blob::source_tree::materialized_source_tree_hash;
use crate::routing::input_resolver::{
    CAPSULE_LOCK_FILE_NAME, DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME,
};

/// The Capsule manifest file name at the selected root.
const CAPSULE_MANIFEST_FILE_NAME: &str = "capsule.toml";

/// The control files of a selected capsule root (ADR-014 §1): the manifest
/// plus the ONE selected canonical lock path, if any. These are the only
/// paths the projection excludes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapsuleControlFiles {
    pub manifest: PathBuf,
    pub lock: Option<PathBuf>,
}

/// Resolves the control files at `selected_root` (§1 steps 2–3): the manifest
/// must exist as a regular file, and the lock path is selected by exact path —
/// `capsule.lock` canonical, `ato.lock.json` deprecated alias, coexistence
/// fail-closed, neither = `None`. No content is read.
///
/// Presence is decided with `symlink_metadata` (fail closed): a symlink or
/// directory under a lock name still counts as present for the coexistence
/// check. In the projection flow the A1v2 pass (step 1) has already rejected
/// symlinks and special nodes before this runs.
pub fn resolve_capsule_control_files(
    selected_root: &Path,
) -> Result<CapsuleControlFiles, CapsuleProgramError> {
    let manifest = selected_root.join(CAPSULE_MANIFEST_FILE_NAME);
    match fs::symlink_metadata(&manifest) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(metadata) => {
            return Err(CapsuleProgramError::SourceProjection(format!(
                "{} must be a regular file, found {}",
                manifest.display(),
                node_kind(metadata.file_type()),
            )));
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Err(CapsuleProgramError::SourceProjection(format!(
                "required manifest {} does not exist",
                manifest.display(),
            )));
        }
        Err(source) => return Err(projection_io("inspect manifest", &manifest, source)),
    }

    let canonical = selected_root.join(CAPSULE_LOCK_FILE_NAME);
    let alias = selected_root.join(DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME);
    let lock = match (
        fs::symlink_metadata(&canonical).is_ok(),
        fs::symlink_metadata(&alias).is_ok(),
    ) {
        (true, true) => {
            return Err(CapsuleProgramError::SourceProjection(format!(
                "both {canonical} and {alias} exist at {root}; no automatic lock-path \
                 choice is made — remove one of the two files (keep {canonical})",
                canonical = CAPSULE_LOCK_FILE_NAME,
                alias = DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME,
                root = selected_root.display(),
            )));
        }
        (true, false) => Some(canonical),
        (false, true) => Some(alias),
        (false, false) => None,
    };

    Ok(CapsuleControlFiles { manifest, lock })
}

/// Derives the pinned [`ProgramSourceContract`] of `selected_root` by the §1
/// six-step order. The projected copy lives in a process-private temporary
/// directory that is removed before returning; only the digest escapes.
pub fn project_program_source(
    selected_root: &Path,
) -> Result<ProgramSourceContract, CapsuleProgramError> {
    // Step 1: A1v2 admissibility over the ORIGINAL tree, control files
    // included. The hash is discarded — this is the gate, not the digest.
    materialized_source_tree_hash(selected_root).map_err(|source| {
        CapsuleProgramError::SourceProjection(format!(
            "A1v2 admissibility rejected the source tree at {}: {source}",
            selected_root.display(),
        ))
    })?;

    // Steps 2–3: manifest presence + lock-path selection.
    let control_files = resolve_capsule_control_files(selected_root)?;

    // Steps 4–5: materialize the projected copy, excluding exactly the
    // resolved control paths (all root-level, so full-path equality suffices).
    let projected = TempDir::new().map_err(|source| {
        CapsuleProgramError::SourceProjection(format!(
            "failed to create projection directory: {source}"
        ))
    })?;
    let mut excluded: Vec<&Path> = vec![control_files.manifest.as_path()];
    if let Some(lock) = control_files.lock.as_deref() {
        excluded.push(lock);
    }
    copy_tree_excluding(selected_root, projected.path(), &excluded)?;

    // Step 6: the frozen A1 digest over the projected root.
    let blob_hash = materialized_source_tree_hash(projected.path()).map_err(|source| {
        CapsuleProgramError::SourceProjection(format!(
            "failed to hash the projected source tree: {source}"
        ))
    })?;
    let digest = ProgramSourceDigest::parse(&blob_hash)?;

    Ok(ProgramSourceContract {
        digest,
        projection_schema: ProgramSourceProjectionSchemaV1,
    })
}

/// Copies `source_dir` into `dest_dir` recursively, skipping entries whose
/// full path is in `excluded`. `fs::copy` preserves unix permission bits, so
/// the A1 executable-bit identity survives projection. The A1v2 pass has
/// already rejected symlinks and special nodes; any encountered here means
/// the tree changed after the gate, and the projection fails closed.
fn copy_tree_excluding(
    source_dir: &Path,
    dest_dir: &Path,
    excluded: &[&Path],
) -> Result<(), CapsuleProgramError> {
    let entries = fs::read_dir(source_dir)
        .map_err(|source| projection_io("read directory", source_dir, source))?;
    for entry in entries {
        let entry = entry.map_err(|source| projection_io("read directory", source_dir, source))?;
        let path = entry.path();
        if excluded.contains(&path.as_path()) {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|source| projection_io("inspect entry", &path, source))?;
        let file_type = metadata.file_type();
        let destination = dest_dir.join(entry.file_name());
        if file_type.is_dir() {
            fs::create_dir(&destination)
                .map_err(|source| projection_io("create directory", &destination, source))?;
            copy_tree_excluding(&path, &destination, excluded)?;
        } else if file_type.is_file() {
            fs::copy(&path, &destination)
                .map_err(|source| projection_io("copy file", &path, source))?;
        } else {
            return Err(CapsuleProgramError::SourceProjection(format!(
                "unexpected {} at {} during projection (tree changed after the \
                 admissibility pass)",
                node_kind(file_type),
                path.display(),
            )));
        }
    }
    Ok(())
}

fn node_kind(file_type: fs::FileType) -> &'static str {
    if file_type.is_dir() {
        "a directory"
    } else if file_type.is_symlink() {
        "a symlink"
    } else {
        "an unsupported node type"
    }
}

fn projection_io(action: &str, path: &Path, source: std::io::Error) -> CapsuleProgramError {
    CapsuleProgramError::SourceProjection(format!(
        "failed to {action} {}: {source}",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_file(root: &Path, rel: &str, contents: &[u8]) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    /// A base tree that deliberately contains control-file NAMES at nested
    /// paths — those are ordinary source and must be hashed.
    fn write_base_tree(root: &Path) {
        write_file(root, "capsule.toml", b"[capsule]\nname = \"demo\"\n");
        write_file(root, "src/main.py", b"print('hi')\n");
        write_file(root, "fixtures/ato.lock.json", b"{\"fixture\": true}\n");
        write_file(
            root,
            "examples/capsule.toml",
            b"[capsule]\nname = \"example\"\n",
        );
    }

    /// A lock body embedding a program_identity-shaped payload: even a lock
    /// that stores the derived id must not reach the digest preimage.
    const LOCK_BODY: &[u8] = br#"{
  "schema": "ato.lock/v1",
  "program_identity": {
    "capsule_program_id": "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "program_contract": { "schema": "ato.capsule-program/v1" }
  }
}
"#;

    #[test]
    fn projection_digest_is_fixed_point_across_lock_spellings() {
        let no_lock = TempDir::new().unwrap();
        write_base_tree(no_lock.path());

        let canonical_lock = TempDir::new().unwrap();
        write_base_tree(canonical_lock.path());
        write_file(canonical_lock.path(), CAPSULE_LOCK_FILE_NAME, LOCK_BODY);

        let alias_lock = TempDir::new().unwrap();
        write_base_tree(alias_lock.path());
        write_file(
            alias_lock.path(),
            DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME,
            LOCK_BODY,
        );

        let base = project_program_source(no_lock.path()).unwrap();
        let with_canonical = project_program_source(canonical_lock.path()).unwrap();
        let with_alias = project_program_source(alias_lock.path()).unwrap();

        assert_eq!(base.digest, with_canonical.digest);
        assert_eq!(base.digest, with_alias.digest);
        assert_eq!(base, with_canonical);
        assert_eq!(base, with_alias);
    }

    #[test]
    fn rejects_coexisting_lock_names_at_root() {
        let tmp = TempDir::new().unwrap();
        write_base_tree(tmp.path());
        write_file(tmp.path(), CAPSULE_LOCK_FILE_NAME, LOCK_BODY);
        write_file(
            tmp.path(),
            DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME,
            LOCK_BODY,
        );

        let err = project_program_source(tmp.path()).unwrap_err();
        let CapsuleProgramError::SourceProjection(message) = &err else {
            panic!("expected SourceProjection, got {err:?}");
        };
        assert!(message.contains(CAPSULE_LOCK_FILE_NAME), "{message}");
        assert!(
            message.contains(DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME),
            "{message}"
        );
    }

    #[test]
    fn resolve_selects_exactly_one_lock_path() {
        let neither = TempDir::new().unwrap();
        write_base_tree(neither.path());
        let control = resolve_capsule_control_files(neither.path()).unwrap();
        assert_eq!(
            control.manifest,
            neither.path().join(CAPSULE_MANIFEST_FILE_NAME)
        );
        assert_eq!(control.lock, None);

        let canonical = TempDir::new().unwrap();
        write_base_tree(canonical.path());
        write_file(canonical.path(), CAPSULE_LOCK_FILE_NAME, LOCK_BODY);
        let control = resolve_capsule_control_files(canonical.path()).unwrap();
        assert_eq!(
            control.lock,
            Some(canonical.path().join(CAPSULE_LOCK_FILE_NAME))
        );

        let alias = TempDir::new().unwrap();
        write_base_tree(alias.path());
        write_file(
            alias.path(),
            DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME,
            LOCK_BODY,
        );
        let control = resolve_capsule_control_files(alias.path()).unwrap();
        assert_eq!(
            control.lock,
            Some(alias.path().join(DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME))
        );
    }

    #[test]
    fn nested_control_file_names_are_ordinary_source() {
        let tmp = TempDir::new().unwrap();
        write_base_tree(tmp.path());
        let baseline = project_program_source(tmp.path()).unwrap().digest;

        write_file(tmp.path(), "fixtures/ato.lock.json", b"{\"fixture\": 2}\n");
        let after_lock_fixture = project_program_source(tmp.path()).unwrap().digest;
        assert_ne!(baseline, after_lock_fixture);

        write_file(
            tmp.path(),
            "examples/capsule.toml",
            b"[capsule]\nname = \"changed\"\n",
        );
        let after_manifest_fixture = project_program_source(tmp.path()).unwrap().digest;
        assert_ne!(after_lock_fixture, after_manifest_fixture);
    }

    #[cfg(unix)]
    #[test]
    fn executable_bit_flip_changes_projection_digest() {
        use std::os::unix::fs::PermissionsExt;

        let plain = TempDir::new().unwrap();
        write_base_tree(plain.path());
        write_file(plain.path(), "bin/run", b"#!/bin/sh\necho hi\n");
        fs::set_permissions(
            plain.path().join("bin/run"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();

        let executable = TempDir::new().unwrap();
        write_base_tree(executable.path());
        write_file(executable.path(), "bin/run", b"#!/bin/sh\necho hi\n");
        fs::set_permissions(
            executable.path().join("bin/run"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();

        let plain_digest = project_program_source(plain.path()).unwrap().digest;
        let executable_digest = project_program_source(executable.path()).unwrap().digest;
        assert_ne!(plain_digest, executable_digest);
    }

    #[cfg(unix)]
    #[test]
    fn projected_copy_preserves_executable_bit() {
        use std::os::unix::fs::PermissionsExt;

        let source = TempDir::new().unwrap();
        write_file(source.path(), "bin/run", b"#!/bin/sh\necho hi\n");
        fs::set_permissions(
            source.path().join("bin/run"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();

        let destination = TempDir::new().unwrap();
        copy_tree_excluding(source.path(), destination.path(), &[]).unwrap();

        let mode = fs::metadata(destination.path().join("bin/run"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o111,
            0o111,
            "fs::copy must preserve the executable bit, got mode {mode:o}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_named_capsule_lock_rejected_by_admissibility_pass() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().unwrap();
        write_base_tree(tmp.path());
        symlink("capsule.toml", tmp.path().join(CAPSULE_LOCK_FILE_NAME)).unwrap();

        let err = project_program_source(tmp.path()).unwrap_err();
        let CapsuleProgramError::SourceProjection(message) = &err else {
            panic!("expected SourceProjection, got {err:?}");
        };
        assert!(
            message.contains("A1v2 admissibility") && message.contains("symlink"),
            "a control-file symlink must fail the step-1 gate, not be excluded: {message}"
        );
    }

    #[test]
    fn missing_root_manifest_is_rejected() {
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "src/main.py", b"print('hi')\n");

        let err = project_program_source(tmp.path()).unwrap_err();
        let CapsuleProgramError::SourceProjection(message) = &err else {
            panic!("expected SourceProjection, got {err:?}");
        };
        assert!(message.contains(CAPSULE_MANIFEST_FILE_NAME), "{message}");
        assert!(message.contains("does not exist"), "{message}");
    }

    #[test]
    fn root_manifest_directory_is_rejected() {
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "src/main.py", b"print('hi')\n");
        fs::create_dir(tmp.path().join(CAPSULE_MANIFEST_FILE_NAME)).unwrap();

        let err = project_program_source(tmp.path()).unwrap_err();
        let CapsuleProgramError::SourceProjection(message) = &err else {
            panic!("expected SourceProjection, got {err:?}");
        };
        assert!(message.contains("must be a regular file"), "{message}");
        assert!(message.contains("a directory"), "{message}");
    }
}
