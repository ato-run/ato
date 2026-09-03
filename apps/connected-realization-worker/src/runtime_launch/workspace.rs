//! Materializing the workspace a launch runs from.
//!
//! `LaunchWorkspaceV1::materialization_ref` is a content address, and this
//! turns it into a directory. The Runner never receives a path, a bucket name
//! or a URL for it — it names content by digest and the control plane
//! resolves it, the same shape the state artifacts use.
//!
//! ## Why this reuses the state artifact format
//!
//! A workspace and a state attachment are both "a filesystem tree, addressed
//! by the digest of its bytes". Giving the workspace a second format would
//! mean a second packer, a second traversal check and a second set of limits —
//! three more places for the two to disagree about what a tree is. So
//! `pack_state_tree` / `unpack_state_tree` serve both, and the containment and
//! atomicity properties they already carry apply here unchanged.
//!
//! The difference is direction, and it is enforced by the mount rather than by
//! the format: a workspace is bound read-only into the sandbox, so nothing the
//! workload does to `/app` can be packed back.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::state_artifact::{state_artifact_digest, unpack_state_tree};

/// Where a lease's workspace is materialized.
pub fn workspace_root(lease_root: &Path) -> PathBuf {
    lease_root.join("workspace")
}

/// Fetching workspace bytes by content address.
///
/// A trait for the same reason the state transport is one: the whole
/// materialization can then be exercised without a network.
pub trait WorkspaceTransport {
    fn download(&self, materialization_ref: &str) -> Result<Vec<u8>>;
}

/// The real transport: a lease-scoped, bearer-authenticated request.
pub struct LeaseWorkspaceTransport {
    client: reqwest::blocking::Client,
    base: String,
    lease_id: String,
    token: String,
}

impl LeaseWorkspaceTransport {
    pub fn new(
        client: reqwest::blocking::Client,
        base: impl Into<String>,
        lease_id: impl Into<String>,
        token: impl Into<String>,
    ) -> Self {
        Self {
            client,
            base: base.into(),
            lease_id: lease_id.into(),
            token: token.into(),
        }
    }
}

impl WorkspaceTransport for LeaseWorkspaceTransport {
    fn download(&self, materialization_ref: &str) -> Result<Vec<u8>> {
        let response = self
            .client
            .get(format!(
                "{}/v1/runner-leases/{}/workspace/{}",
                self.base, self.lease_id, materialization_ref
            ))
            .bearer_auth(&self.token)
            .send()?
            .error_for_status()
            .context("failed to download the workspace artifact")?;
        Ok(response.bytes()?.to_vec())
    }
}

/// Materialize `materialization_ref` into a fresh directory under the lease.
///
/// Refuses anything that is not a content address. A synthetic or all-zero
/// digest would materialize an empty workspace and the Run would fail somewhere
/// far away from the cause, so it is refused by name here.
pub fn materialize_workspace(
    transport: &dyn WorkspaceTransport,
    materialization_ref: &str,
    lease_root: &Path,
) -> Result<PathBuf> {
    if !is_content_address(materialization_ref) {
        bail!(
            "workspace materialization_ref {materialization_ref:?} is not a content address; \
             refusing to launch from an unaddressable workspace"
        );
    }
    if is_placeholder_digest(materialization_ref) {
        bail!(
            "workspace materialization_ref is an all-zero placeholder; a Run must name real \
             content"
        );
    }

    let bytes = transport.download(materialization_ref)?;
    let observed = state_artifact_digest(&bytes);
    if observed != materialization_ref {
        // Re-verified even though the control plane resolved it: the bytes
        // crossed a network, and a workspace that is not what it claims to be
        // is code the Run would execute.
        bail!(
            "workspace artifact digest mismatch: expected {materialization_ref}, computed \
             {observed}"
        );
    }

    let root = workspace_root(lease_root);
    unpack_state_tree(&bytes, materialization_ref, &root)
        .context("failed to materialize the workspace")?;
    Ok(root)
}

fn is_content_address(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn is_placeholder_digest(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|digest| digest.bytes().all(|byte| byte == b'0'))
}

#[cfg(test)]
mod tests {
    use super::super::state_artifact::pack_state_tree;
    use super::*;

    struct Bytes(Vec<u8>);
    impl WorkspaceTransport for Bytes {
        fn download(&self, _reference: &str) -> Result<Vec<u8>> {
            Ok(self.0.clone())
        }
    }

    struct NeverCalled;
    impl WorkspaceTransport for NeverCalled {
        fn download(&self, _reference: &str) -> Result<Vec<u8>> {
            panic!("an unaddressable workspace must not be fetched")
        }
    }

    fn fixture() -> (tempfile::TempDir, Vec<u8>, String) {
        let source = tempfile::tempdir().expect("tempdir");
        std::fs::write(source.path().join("app.py"), "print('hi')\n").expect("write");
        let artifact = pack_state_tree(source.path()).expect("packs");
        let digest = artifact.digest().to_owned();
        (source, artifact.bytes().to_vec(), digest)
    }

    #[test]
    fn a_workspace_materializes_from_its_content_address() {
        let (_source, bytes, digest) = fixture();
        let lease_root = tempfile::tempdir().expect("tempdir");
        let root =
            materialize_workspace(&Bytes(bytes), &digest, lease_root.path()).expect("materializes");
        assert_eq!(
            std::fs::read_to_string(root.join("app.py")).expect("read"),
            "print('hi')\n"
        );
    }

    #[test]
    fn a_zero_placeholder_is_refused_before_anything_is_fetched() {
        // The placeholder the internal launch route used to send. It would
        // have materialized an empty workspace and failed far from the cause.
        let lease_root = tempfile::tempdir().expect("tempdir");
        let error = materialize_workspace(
            &NeverCalled,
            &format!("sha256:{}", "0".repeat(64)),
            lease_root.path(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("placeholder"), "{error}");
    }

    #[test]
    fn a_reference_that_is_not_a_content_address_is_refused() {
        let lease_root = tempfile::tempdir().expect("tempdir");
        for reference in ["/var/lib/ato/workspace", "swm_dispatch", "latest"] {
            let error =
                materialize_workspace(&NeverCalled, reference, lease_root.path()).unwrap_err();
            assert!(
                error.to_string().contains("not a content address"),
                "{reference} was not refused: {error}"
            );
        }
    }

    #[test]
    fn bytes_that_do_not_match_the_reference_are_refused() {
        let (_source, _bytes, digest) = fixture();
        let lease_root = tempfile::tempdir().expect("tempdir");
        // A workspace that is not what it claims to be is code the Run would
        // execute.
        let error = materialize_workspace(&Bytes(b"tampered".to_vec()), &digest, lease_root.path())
            .unwrap_err();
        assert!(error.to_string().contains("digest mismatch"), "{error}");
        assert!(!workspace_root(lease_root.path()).exists());
    }
}
