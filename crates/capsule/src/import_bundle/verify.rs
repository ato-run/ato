//! The two-stage verification API, and the proof-carrying types it mints.
//!
//! ```text
//! verify_capsule_envelope(..)            -> VerifiedCapsuleEnvelope
//! derive_imported_capsule(envelope)      -> VerifiedCapsuleImport
//! VerifiedCapsuleImport::into_workspace() -> ImportedCapsuleWorkspace
//! ```
//!
//! Every field of all three types is private and none has a public constructor,
//! so "this bundle was verified" is a claim only this module can make. A caller
//! holding raw bytes cannot manufacture a `VerifiedCapsuleEnvelope` to hand
//! downstream.
//!
//! # Verification order
//!
//! Fixed, and each step exists because doing it later would be exploitable:
//!
//! ```text
//! 1. concurrency slot                     — before any disk is touched
//! 2. whole-bundle SHA-256 vs expected     — before ANY parsing (Store path)
//! 3. dispatch: is there a root index.json — absent ⇒ hand off to v2, never parse as v3
//! 4. outer structural parse + staging     — exactly-4 allowlist, entry-kind gate
//! 5. index.json strict parse + JCS check  — fixes the signing target
//! 6. per-member digest + size             — RFC §index.json: structural, before any
//!                                           signature or trust decision
//! 7. signature.json strict parse
//! 8. index_digest recompute, then Ed25519 over the domain-separated message
//! 9. SignerTrust resolution (origin-scoped) and policy enforcement
//! ```
//!
//! Step 6 sits before steps 7-8 deliberately. RFC §`index.json` calls the
//! member digest/size invariants "all enforced structurally, before any
//! signature or trust decision", and its own golden-vector list says a bundle
//! whose `capsule.toml` was tampered with post-signing is "a manifest member
//! digest mismatch (caught first, before signature verification is even
//! reached)". Both orders are sound — the signature covers the index, and the
//! index covers the members — but only this one produces the error the contract
//! names.

use std::fs;
use std::io::{Read, Seek};
use std::path::Path;

use tempfile::TempDir;

use super::CapsuleImportError;
use super::index::{
    CapsuleIndexV1, IndexMember, MANIFEST_MEMBER_PATH, SOURCE_MEMBER_PATH, Sha256Digest,
    parse_index_json,
};
use super::policy::{CapsuleImportPolicy, ImportSlot};
use super::reader::{
    BundleFormat, INDEX_MEMBER_PATH, SIGNATURE_MEMBER_PATH, StagedMember, StagedOuterMembers,
    classify_bundle_format, hash_whole_stream, stage_v3_outer_members,
};
use super::signature::{CapsuleIndexSignatureV1, ClaimedIssuer, DidKey, parse_signature_json};
use super::trust::{CapsuleImportContext, CapsuleTrustPolicy, SignerTrust};
use crate::blob::materialize_source_archive;
use crate::capsule_program_contract::{CapsuleProgramId, derive_capsule_program_contract};
use crate::input_resolver::{CAPSULE_LOCK_FILE_NAME, DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME};
use crate::program_source_projection::{
    VerifiedPinnedSourceMaterialization, extract_source_archive,
    materialize_program_source_projection,
};

// ─────────────────────────────────────────────────────────────────────────────
// Stage 1 — the envelope
// ─────────────────────────────────────────────────────────────────────────────

/// A v3 bundle whose container, index, member digests, and signature have all
/// been verified, with its four members staged in a process-private directory.
///
/// Outer-manifest authority has **not** yet been applied and no
/// `capsule_program_id` has been derived — that is
/// [`derive_imported_capsule`]'s job. The split exists so the byte-level
/// verification a local file and a Store download share is provably one code
/// path, not two similar ones.
///
/// Dropping this value removes the staging directory, so every failure path
/// after it is constructed cleans up unconditionally.
pub struct VerifiedCapsuleEnvelope {
    staged: StagedOuterMembers,
    index: CapsuleIndexV1,
    signature: CapsuleIndexSignatureV1,
    signer_trust: SignerTrust,
    bundle_digest: Sha256Digest,
    /// Held, never read: it releases the importer's concurrency slot on drop,
    /// so the slot covers the staging lifetime rather than just the parse.
    _import_slot: ImportSlot,
}

/// No derived `Debug`: it would print the staging directory's absolute path out
/// of a private field, which is exactly the process-private path this type
/// withholds.
impl std::fmt::Debug for VerifiedCapsuleEnvelope {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VerifiedCapsuleEnvelope")
            .field("bundle_digest", &self.bundle_digest)
            .field("signer_trust", &self.signer_trust)
            .field("key_id", &self.signature.key_id.as_str())
            .finish_non_exhaustive()
    }
}

impl VerifiedCapsuleEnvelope {
    /// How much trust the signing key carries.
    #[must_use]
    pub fn signer_trust(&self) -> SignerTrust {
        self.signer_trust
    }

    /// SHA-256 of the entire outer `.capsule` byte stream.
    #[must_use]
    pub fn bundle_digest(&self) -> Sha256Digest {
        self.bundle_digest
    }

    /// The signing key's canonical `did:key`.
    #[must_use]
    pub fn key_id(&self) -> &DidKey {
        &self.signature.key_id
    }

    /// The signer's self-declared issuer.
    ///
    /// Display-only. It played no part in [`Self::signer_trust`] — a bundle
    /// claiming [`ClaimedIssuer::AtoStore`] and signed by an unpinned key still
    /// reports [`SignerTrust::UntrustedKey`].
    #[must_use]
    pub fn claimed_issuer(&self) -> ClaimedIssuer {
        self.signature.claimed_issuer
    }

    /// The verified member manifest.
    #[must_use]
    pub fn index(&self) -> &CapsuleIndexV1 {
        &self.index
    }

    /// The outer staging root, so a test can prove it is removed on every exit
    /// path. `#[cfg(test)]` — no consumer may hold a path into private staging.
    #[cfg(test)]
    pub(crate) fn staging_root_for_test(&self) -> &Path {
        self.staged.staging_root()
    }
}

/// Verify a `.capsule` byte stream as a v3 bundle.
///
/// See the module docs for the fixed step order.
///
/// # Errors
///
/// * [`CapsuleImportError::NotV3Bundle`] — no root `index.json`; the caller must
///   hand the same bytes to the existing v2 reader. This is a dispatch signal,
///   not a verdict on the archive.
/// * [`CapsuleImportError::BundleDigestMismatch`] — the bytes are not the bytes
///   the caller asked for.
/// * [`CapsuleImportError::CapsuleInvalid`] / [`CapsuleImportError::SignatureInvalid`]
///   — structural or cryptographic violation.
/// * [`CapsuleImportError::UntrustedSigner`] — valid signature, insufficient
///   trust for this path.
/// * [`CapsuleImportError::ResourceBudgetExceeded`] /
///   [`CapsuleImportError::InsufficientLocalStorage`] — policy, not validity.
pub fn verify_capsule_envelope<R: Read + Seek>(
    mut reader: R,
    context: CapsuleImportContext,
    trust_policy: &CapsuleTrustPolicy,
    import_policy: &CapsuleImportPolicy,
) -> Result<VerifiedCapsuleEnvelope, CapsuleImportError> {
    // 1. Concurrency slot, before any disk is touched.
    let import_slot = import_policy.acquire_import_slot()?;

    // 2. Whole-bundle digest, before ANY v3 parsing. A Store-fetched bundle
    //    that is not the artifact the API named is refused without its contents
    //    ever being interpreted.
    let bundle_digest = hash_whole_stream(&mut reader)?;
    if let Some(expected) = context.expected_bundle_digest()
        && bundle_digest != *expected
    {
        return Err(CapsuleImportError::BundleDigestMismatch {
            expected: expected.to_string(),
            actual: bundle_digest.to_string(),
        });
    }

    // 3. Dispatch. `index.json` absent is not a v3 rejection — it is a hand-off
    //    to the v2 reader, which owns its own validity rules.
    if classify_bundle_format(&mut reader)? == BundleFormat::V2Legacy {
        return Err(CapsuleImportError::NotV3Bundle);
    }
    reader
        .rewind()
        .map_err(|source| CapsuleImportError::io("rewind the bundle stream", source))?;

    // 4. Outer structural parse: exactly four regular-file members, entry kind
    //    checked before the name, aliases refused by raw-byte comparison.
    let staged = stage_v3_outer_members(&mut reader, import_policy)?;

    // 5. index.json: strict parse, then the JCS self-consistency check that
    //    makes these bytes a well-defined signing target.
    let index_bytes = staged.member(INDEX_MEMBER_PATH).read_bytes()?;
    let index = parse_index_json(&index_bytes)?;

    // 6. Every declared digest and size must match the bytes actually staged.
    verify_declared_member(index.manifest_member(), staged.member(MANIFEST_MEMBER_PATH))?;
    verify_declared_member(index.source_member(), staged.member(SOURCE_MEMBER_PATH))?;

    // 7. signature.json: same strict-parsing treatment as index.json. Absence is
    //    a rejection for v3 — there is no degrade-to-unsigned path — and the
    //    exactly-four allowlist already guaranteed presence by this point.
    let signature_bytes = staged.member(SIGNATURE_MEMBER_PATH).read_bytes()?;
    let signature = parse_signature_json(&signature_bytes)?;

    // 8. index_digest recompute (first), then Ed25519 over the domain-separated
    //    message.
    signature.verify_over_index(&index_bytes)?;

    // 9. Trust, resolved against ONLY this call's origin.
    let signer_trust = trust_policy.resolve(&context, &signature.key_id);
    enforce_trust(&context, trust_policy, signer_trust, &signature)?;

    Ok(VerifiedCapsuleEnvelope {
        staged,
        index,
        signature,
        signer_trust,
        bundle_digest,
        _import_slot: import_slot,
    })
}

fn verify_declared_member(
    declared: &IndexMember,
    staged: &StagedMember,
) -> Result<(), CapsuleImportError> {
    let actual_digest = staged.digest();
    if actual_digest != declared.sha256 {
        return Err(CapsuleImportError::invalid(format!(
            "outer member {:?} hashes to {actual_digest}, but index.json declares {}",
            declared.path, declared.sha256
        )));
    }
    // String comparison, never a numeric parse: `size_bytes` is untrusted input
    // and the format admits values no `u64` can hold.
    let actual_size = staged.size().to_string();
    if actual_size != declared.size_bytes.as_str() {
        return Err(CapsuleImportError::invalid(format!(
            "outer member {:?} is {actual_size} bytes, but index.json declares {}",
            declared.path,
            declared.size_bytes.as_str()
        )));
    }
    Ok(())
}

/// Apply the caller's policy to an already-resolved trust classification.
///
/// Store Install is unconditional: RFC §"Slice 1 signer policy" — "a bundle
/// fetched via Store Install that resolves to `untrusted_key` is rejected
/// outright — there is no confirmation prompt on this path, because the whole
/// point of Store Install is that the API is the trust anchor."
///
/// Local import defers to the caller. This module never prompts; setting
/// [`CapsuleTrustPolicy::accepting_untrusted_local_signers`] means the caller's
/// UI owns the confirmation and wants the classification handed back instead.
fn enforce_trust(
    context: &CapsuleImportContext,
    trust_policy: &CapsuleTrustPolicy,
    signer_trust: SignerTrust,
    signature: &CapsuleIndexSignatureV1,
) -> Result<(), CapsuleImportError> {
    match context {
        CapsuleImportContext::Store { api_origin, .. } => {
            if signer_trust == SignerTrust::TrustedStore {
                return Ok(());
            }
            Err(CapsuleImportError::UntrustedSigner(format!(
                "bundle fetched from {} is signed by {}, which matches no key pinned for that \
                 origin (it resolves to {}); the bundle's own claimed_issuer {:?} is \
                 self-declared and carries no weight",
                api_origin.as_str(),
                signature.key_id,
                signer_trust.as_str(),
                signature.claimed_issuer.as_str(),
            )))
        }
        CapsuleImportContext::LocalFile { .. } => {
            if signer_trust != SignerTrust::UntrustedKey
                || trust_policy.accepts_untrusted_with_confirmation()
            {
                return Ok(());
            }
            Err(CapsuleImportError::UntrustedSigner(format!(
                "local bundle is validly signed by {}, but that key carries no established \
                 trust and this importer was not configured to accept an unrecognized signer \
                 with explicit confirmation",
                signature.key_id,
            )))
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Stage 2 — outer-manifest authority and program identity
// ─────────────────────────────────────────────────────────────────────────────

/// A verified bundle with the outer manifest applied and
/// `capsule_program_id` re-derived from (outer manifest, control-file-excluded
/// source projection).
pub struct VerifiedCapsuleImport {
    workspace: TempDir,
    capsule_program_id: CapsuleProgramId,
    excluded_control_files: Vec<String>,
    signer_trust: SignerTrust,
    bundle_digest: Sha256Digest,
}

impl std::fmt::Debug for VerifiedCapsuleImport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VerifiedCapsuleImport")
            .field("capsule_program_id", &self.capsule_program_id)
            .field("excluded_control_files", &self.excluded_control_files)
            .field("signer_trust", &self.signer_trust)
            .field("bundle_digest", &self.bundle_digest)
            .finish_non_exhaustive()
    }
}

impl VerifiedCapsuleImport {
    /// The re-derived program identity.
    #[must_use]
    pub fn capsule_program_id(&self) -> &CapsuleProgramId {
        &self.capsule_program_id
    }

    /// The inner control files the projection withheld, root-relative and
    /// sorted.
    #[must_use]
    pub fn excluded_control_files(&self) -> &[String] {
        &self.excluded_control_files
    }

    /// How much trust the signing key carried.
    #[must_use]
    pub fn signer_trust(&self) -> SignerTrust {
        self.signer_trust
    }

    /// SHA-256 of the outer `.capsule` byte stream this import came from.
    #[must_use]
    pub fn bundle_digest(&self) -> Sha256Digest {
        self.bundle_digest
    }

    /// Hand over the runnable workspace.
    ///
    /// Infallible: the workspace was fully materialized inside
    /// [`derive_imported_capsule`], so there is no work left that could fail
    /// here — and therefore no window in which a caller holds a "verified
    /// import" whose workspace does not exist.
    #[must_use]
    pub fn into_workspace(self) -> ImportedCapsuleWorkspace {
        ImportedCapsuleWorkspace {
            workspace: self.workspace,
            capsule_program_id: self.capsule_program_id,
            signer_trust: self.signer_trust,
        }
    }
}

/// The runnable workspace an import produces: the control-file-excluded source
/// projection plus the **outer** `capsule.toml`, and no lock of any kind.
///
/// No lock is written on purpose. RFC §"Container layout": a cloud-built lock
/// can carry cloud-side Execution Contract and platform-specific resolution that
/// must not silently govern a local install on a different machine, so the local
/// resolver generates `capsule.lock` after import, inside the existing install
/// pipeline. An inner `capsule.lock` / `ato.lock.json` that rode along in the
/// source archive is excluded by the projection and never reaches here.
///
/// Owns its [`TempDir`]: the workspace lives exactly as long as this value.
pub struct ImportedCapsuleWorkspace {
    workspace: TempDir,
    capsule_program_id: CapsuleProgramId,
    signer_trust: SignerTrust,
}

impl std::fmt::Debug for ImportedCapsuleWorkspace {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ImportedCapsuleWorkspace")
            .field("capsule_program_id", &self.capsule_program_id)
            .field("signer_trust", &self.signer_trust)
            .finish_non_exhaustive()
    }
}

impl ImportedCapsuleWorkspace {
    /// The workspace root, for the install pipeline to consume.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.workspace.path()
    }

    /// The verified program identity of what is in that workspace.
    #[must_use]
    pub fn capsule_program_id(&self) -> &CapsuleProgramId {
        &self.capsule_program_id
    }

    /// How much trust the signing key carried.
    #[must_use]
    pub fn signer_trust(&self) -> SignerTrust {
        self.signer_trust
    }
}

/// Apply outer-manifest authority and derive `capsule_program_id`.
///
/// Everything happens inside process-private staging directories this call owns:
///
/// ```text
/// 1. re-verify the source member's digest (precondition re-assert)
/// 2. extract source.tar.zst into private staging
/// 3. existing ato.source-archive/v1 admissibility runs during that extraction
/// 4. identify root control files: capsule.toml, capsule.lock, ato.lock.json
/// 5. exclude them from the projection input set
/// 6. parse and normalize the OUTER capsule.toml
/// 7. compute the control-file-excluded source projection
/// 8. derive capsule_program_id via the existing SSOT
/// 9. write the projection + the outer capsule.toml into the workspace, no lock
/// ```
///
/// Steps 4-8 are not re-implemented here. The outer manifest is written over the
/// extracted root's `capsule.toml`, the tree is re-archived, and the existing
/// single public mint
/// ([`VerifiedPinnedSourceMaterialization::from_source_archive`]) plus
/// [`derive_capsule_program_contract`] do the rest. That is what makes the
/// identity provably the same function the rest of the system uses: the manifest
/// is authoritative because it IS the root manifest by the time the SSOT reads
/// it, and the inner one contributed nothing because the projection excludes the
/// root manifest by path regardless of content.
///
/// # Errors
///
/// [`CapsuleImportError::CapsuleInvalid`] for an inadmissible source tree —
/// including the pre-existing split-brain rule when the archive carries both
/// `capsule.lock` and `ato.lock.json` at its root.
pub fn derive_imported_capsule(
    envelope: VerifiedCapsuleEnvelope,
) -> Result<VerifiedCapsuleImport, CapsuleImportError> {
    let VerifiedCapsuleEnvelope {
        staged,
        index,
        signer_trust,
        bundle_digest,
        signature: _signature,
        _import_slot,
    } = envelope;

    // 1. Re-assert the source member's digest. Already checked in
    //    verify_capsule_envelope; re-checked here because this function is the
    //    one that turns those bytes into an executable tree, and a precondition
    //    worth having is worth re-asserting at the boundary that depends on it.
    let source_member = staged.member(SOURCE_MEMBER_PATH);
    let source_bytes_digest =
        Sha256Digest::of_bytes(&fs::read(source_member.path()).map_err(|source| {
            CapsuleImportError::io("re-read the staged source archive", source)
        })?);
    if source_bytes_digest != index.source_member().sha256 {
        return Err(CapsuleImportError::invalid(
            "the staged source archive no longer matches the digest index.json declares",
        ));
    }

    // 2-3. Extraction IS the admissibility gate for the archive's own entries:
    //      `extract_source_archive` refuses symlinks, hardlinks, devices, FIFOs,
    //      escaping paths, and over-cap members before a byte is written. A
    //      control file that is a symlink therefore never reaches step 4.
    let extracted = TempDir::new()
        .map_err(|source| CapsuleImportError::io("create the source extraction dir", source))?;
    extract_source_archive(source_member.path(), extracted.path())
        .map_err(|error| CapsuleImportError::invalid(error.to_string()))?;

    // 4-6. Outer-manifest authority. Writing the outer bytes over the extracted
    //      root's `capsule.toml` is what makes "outer wins" structural rather
    //      than conditional: absent, malformed, or merely different, the inner
    //      file is gone before anything parses a manifest, and the projection
    //      excludes the root manifest by path regardless of its content — so the
    //      digest is identical to importing against an archive that never had
    //      one.
    let manifest_bytes = staged.member(MANIFEST_MEMBER_PATH).read_bytes()?;
    install_outer_manifest(extracted.path(), &manifest_bytes)?;
    reject_split_brain_locks(extracted.path())?;

    // 7-8. The existing SSOT, unchanged: archive the tree so the proof is minted
    //      by construction, then derive.
    let rebuilt = TempDir::new()
        .map_err(|source| CapsuleImportError::io("create the re-archive staging dir", source))?;
    let rebuilt_archive = rebuilt.path().join("source.tar.zst");
    materialize_source_archive(extracted.path(), &rebuilt_archive)
        .map_err(|error| CapsuleImportError::invalid(error.to_string()))?;

    let pinned = VerifiedPinnedSourceMaterialization::from_source_archive(&rebuilt_archive)
        .map_err(|error| CapsuleImportError::invalid(error.to_string()))?;
    let contract = derive_capsule_program_contract(&pinned)
        .map_err(|error| CapsuleImportError::invalid(error.to_string()))?;
    let capsule_program_id = contract
        .compute_capsule_program_id()
        .map_err(|error| CapsuleImportError::invalid(error.to_string()))?;

    // 9. The runnable workspace: the projected tree (control files withheld) plus
    //    the outer manifest, and no lock.
    let workspace = TempDir::new()
        .map_err(|source| CapsuleImportError::io("create the import workspace", source))?;
    let materialized = materialize_program_source_projection(&pinned, workspace.path())
        .map_err(|error| CapsuleImportError::invalid(error.to_string()))?;
    fs::write(workspace.path().join(MANIFEST_MEMBER_PATH), &manifest_bytes).map_err(|source| {
        CapsuleImportError::io("write the outer manifest into the import workspace", source)
    })?;

    Ok(VerifiedCapsuleImport {
        workspace,
        capsule_program_id,
        excluded_control_files: materialized.excluded_control_files,
        signer_trust,
        bundle_digest,
    })
}

/// Overwrite (or create) `<root>/capsule.toml` with the outer, authoritative
/// bytes.
///
/// A non-regular node under that name is a refusal rather than something to
/// clear away: `extract_source_archive` only ever writes regular files and
/// directories, so a directory named `capsule.toml` means the archive declared
/// one, and silently deleting a directory tree to make room for a manifest is
/// not a decision this layer should be making.
fn install_outer_manifest(root: &Path, manifest_bytes: &[u8]) -> Result<(), CapsuleImportError> {
    let manifest_path = root.join(MANIFEST_MEMBER_PATH);
    match fs::symlink_metadata(&manifest_path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(metadata) => {
            return Err(CapsuleImportError::invalid(format!(
                "the source archive carries a {:?} at its root named {MANIFEST_MEMBER_PATH}, \
                 which the outer manifest cannot replace",
                metadata.file_type()
            )));
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(CapsuleImportError::io(
                "inspect the source archive's root manifest",
                source,
            ));
        }
    }
    fs::write(&manifest_path, manifest_bytes).map_err(|source| {
        CapsuleImportError::io("write the outer manifest into the extracted tree", source)
    })
}

/// The pre-existing split-brain-lock rule, un-relaxed by this import path.
///
/// The projection's own [`crate::program_source_projection::resolve_capsule_control_files`]
/// enforces this too, one layer down. It is re-checked here so the refusal names
/// the *bundle's inner archive* — the thing an operator can act on — instead of
/// surfacing as a message about a process-private staging tree they have never
/// seen.
fn reject_split_brain_locks(root: &Path) -> Result<(), CapsuleImportError> {
    let canonical = root.join(CAPSULE_LOCK_FILE_NAME).exists();
    let alias = root.join(DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME).exists();
    if canonical && alias {
        return Err(CapsuleImportError::invalid(format!(
            "the bundle's source archive carries both {CAPSULE_LOCK_FILE_NAME} and \
             {DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME} at its root; two lock files at once is \
             evidence of a corrupted or adversarially constructed tree, and no automatic choice \
             is made between them"
        )));
    }
    Ok(())
}
