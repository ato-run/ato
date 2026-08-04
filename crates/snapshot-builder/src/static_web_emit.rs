//! Static Web emission seam: "extract → produce → upload" for an explicitly
//! declared Static Web output.
//!
//! This is the ONLY place the builder decides between the three outcomes:
//!
//! - **No declared plan** → a complete no-op; the snapshot lane proceeds
//!   unchanged. A built `dist/` never selects this lane on its own.
//! - **Declared plan, output root missing in the built image** →
//!   `STATIC_WEB_OUTPUT_MISSING`; the build FAILS (declaration vs execution
//!   disagreement is a configuration/build failure, never a silent fallback).
//! - **Declared plan, unsafe file / producer failure** → the specific error
//!   (`STATIC_WEB_OUTPUT_UNSAFE`, `STATIC_WEB_BUNDLE_FAILED`); the build fails.
//!
//! The built image tree must be provided as a host-side directory by the
//! caller (the authoring build's exported rootfs); this module never runs a
//! container or a build command.

use std::path::Path;

use crate::static_web_bundle::{
    produce_static_web_bundle, ProducedStaticWebBundle,
};
use crate::static_web_output::{ExtractedStaticWebOutput, StaticWebOutputPlan, extract_static_web_output};
use crate::static_web_transport::{
    StaticWebBlobUpload, StaticWebPrepare, StaticWebTransport, transport_static_web_bundle,
};

/// The upload step collects the immutable blobs from the produced bundle.
pub fn bundle_blobs(
    bundle: &ProducedStaticWebBundle,
    blob_dir: &Path,
) -> Vec<StaticWebBlobUpload> {
    bundle
        .receipt
        .blobs
        .iter()
        .map(|blob| StaticWebBlobUpload {
            digest: blob.digest.clone(),
            size_bytes: blob.size,
            local_path: blob_dir.join(&blob.digest["sha256:".len()..]),
        })
        .collect()
}

/// The outcome of one static web emission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaticWebEmit {
    /// No declared plan — the snapshot lane proceeds unchanged.
    Skipped,
    /// Declared + produced + uploaded; `materialization_id` names it.
    Emitted { materialization_id: String, manifest_digest: String },
}

/// The per-build context a static web emission runs under.
pub struct StaticWebEmitContext<'a> {
    /// The exported, host-side built image tree. Must be a real directory;
    /// extraction copies only the explicitly selected output subtree.
    pub image_root: &'a Path,
    /// Where the immutable `static-web-bundle-v1/` is written before upload.
    pub destination_parent: &'a Path,
    /// Runtime secret canaries the producer scans for (empty = no claim).
    pub runtime_secret_canaries: &'a [&'a [u8]],
    pub job_id: &'a str,
    pub build_config_revision_id: &'a str,
    pub plan_digest: &'a str,
    pub agent_id: &'a str,
    pub transport: &'a dyn StaticWebTransport,
}

/// Drive the static web lane for one build, if and only if the claim declared
/// an explicit `static_web_output`.
pub fn emit_static_web_if_declared(
    effective_build_plan: Option<&serde_json::Value>,
    context: &StaticWebEmitContext<'_>,
) -> anyhow::Result<StaticWebEmit> {
    let Some(plan) =
        StaticWebOutputPlan::from_effective_build_plan_json(effective_build_plan)?
    else {
        // No declaration: snapshot-only, complete no-op.
        return Ok(StaticWebEmit::Skipped);
    };
    emit_static_web(&plan, context)
}

/// The declared-plan path: extract → produce → upload → complete.
pub fn emit_static_web(
    plan: &StaticWebOutputPlan,
    context: &StaticWebEmitContext<'_>,
) -> anyhow::Result<StaticWebEmit> {
    let extracted: ExtractedStaticWebOutput =
        extract_static_web_output(context.image_root, plan)
            .map_err(|error| anyhow::anyhow!("STATIC_WEB_OUTPUT_MISSING: {error}"))?;
    let bundle = produce_static_web_bundle(
        plan,
        extracted.output_root(),
        context.destination_parent,
        context.runtime_secret_canaries,
    )
    .map_err(|error| anyhow::anyhow!("STATIC_WEB_BUNDLE_FAILED: {error}"))?;

    let prepare = StaticWebPrepare {
        agent_id: context.agent_id.to_string(),
        materialization_id: plan.materialization_id.clone(),
        build_config_revision_id: context.build_config_revision_id.to_string(),
        expected_plan_digest: context.plan_digest.to_string(),
        manifest_base64: base64_encode(&bundle.manifest_bytes),
        receipt_base64: base64_encode(&bundle.receipt_bytes),
        manifest_digest: bundle.receipt.manifest_digest.clone(),
        receipt_digest: bundle.receipt_digest.clone(),
    };
    let blobs = bundle_blobs(&bundle, &bundle.bundle_root.join("blobs/sha256"));
    transport_static_web_bundle(context.transport, context.job_id, &prepare, &blobs)
        .map_err(|error| anyhow::anyhow!("STATIC_WEB_UPLOAD_FAILED: {error}"))?;

    Ok(StaticWebEmit::Emitted {
        materialization_id: plan.materialization_id.clone(),
        manifest_digest: bundle.receipt.manifest_digest.clone(),
    })
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::static_web_transport::{
        StaticWebUploadAuthorization, StaticWebBlobUpload,
    };
    use std::cell::RefCell;

    #[derive(Default)]
    struct RecordingTransport {
        prepares: RefCell<u32>,
        completes: RefCell<u32>,
    }

    impl StaticWebTransport for RecordingTransport {
        fn prepare(
            &self,
            _job_id: &str,
            _input: &StaticWebPrepare,
        ) -> Result<super::super::static_web_transport::StaticWebPrepareDecision, super::super::static_web_transport::StaticWebTransportError> {
            *self.prepares.borrow_mut() += 1;
            Ok(super::super::static_web_transport::StaticWebPrepareDecision::Ready {
                materialization_id: "swm_test".to_string(),
                manifest_digest: format!("sha256:{}", "a".repeat(64)),
            })
        }

        fn authorize_uploads(
            &self,
            _job_id: &str,
            _materialization_id: &str,
            blobs: &[StaticWebBlobUpload],
        ) -> Result<Vec<StaticWebUploadAuthorization>, super::super::static_web_transport::StaticWebTransportError> {
            Ok(blobs
                .iter()
                .map(|blob| StaticWebUploadAuthorization {
                    digest: blob.digest.clone(),
                    status: "upload".to_string(),
                    upload_url: Some(format!(
                        "https://r2.test/static/v1/blobs/sha256/{}?X-Amz-Signature=x",
                        &blob.digest["sha256:".len()..]
                    )),
                    required_headers: vec![],
                })
                .collect())
        }

        fn put(
            &self,
            _url: &str,
            _body: &[u8],
            _headers: &[(String, String)],
        ) -> Result<u16, String> {
            Ok(200)
        }

        fn verify_uploads(
            &self,
            _job_id: &str,
            _materialization_id: &str,
            blobs: &[StaticWebBlobUpload],
        ) -> Result<Vec<(String, bool)>, super::super::static_web_transport::StaticWebTransportError> {
            Ok(blobs.iter().map(|b| (b.digest.clone(), true)).collect())
        }

        fn complete(
            &self,
            _job_id: &str,
            _materialization_id: &str,
        ) -> Result<(), super::super::static_web_transport::StaticWebTransportError> {
            *self.completes.borrow_mut() += 1;
            Ok(())
        }
    }

    fn declared_plan_json() -> serde_json::Value {
        serde_json::json!({
            "schema": "ato.effective-build-plan/v1",
            "static_web_output": {
                "schema": "ato.static-web-output-plan/v1",
                "materialization_id": "swm_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "image_output_root": "app/dist",
                "entry_path": "index.html",
                "spa_fallback": true,
                "connect_src": [],
                "producer_contract": "ato.static-web-producer/v1",
            }
        })
    }

    fn build_image_with_dist() -> tempfile::TempDir {
        let image = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(image.path().join("app/dist/assets")).unwrap();
        std::fs::write(image.path().join("app/dist/index.html"), "<main>ok</main>").unwrap();
        std::fs::write(image.path().join("app/dist/assets/app.js"), "console.log('ok')").unwrap();
        image
    }

    #[test]
    fn absent_declaration_is_a_complete_noop() {
        let image = build_image_with_dist();
        let parent = tempfile::tempdir().unwrap();
        let transport = RecordingTransport::default();
        let context = StaticWebEmitContext {
            image_root: image.path(),
            destination_parent: parent.path(),
            runtime_secret_canaries: &[],
            job_id: "job_1",
            build_config_revision_id: "bcrev_1",
            plan_digest: "sha256:abc",
            agent_id: "builder_1",
            transport: &transport,
        };
        let result = emit_static_web_if_declared(
            Some(&serde_json::json!({ "schema": "ato.effective-build-plan/v1" })),
            &context,
        )
        .unwrap();
        assert!(matches!(result, StaticWebEmit::Skipped));
        assert_eq!(*transport.prepares.borrow(), 0);
        // The built dist/ alone never selected the lane.
        assert!(!parent.path().join("static-web-bundle-v1").exists());
    }

    #[test]
    fn declared_but_missing_output_root_fails_the_build() {
        let image = tempfile::tempdir().unwrap(); // no app/dist
        std::fs::create_dir_all(image.path().join("app")).unwrap();
        let parent = tempfile::tempdir().unwrap();
        let transport = RecordingTransport::default();
        let context = StaticWebEmitContext {
            image_root: image.path(),
            destination_parent: parent.path(),
            runtime_secret_canaries: &[],
            job_id: "job_1",
            build_config_revision_id: "bcrev_1",
            plan_digest: "sha256:abc",
            agent_id: "builder_1",
            transport: &transport,
        };
        let err = emit_static_web_if_declared(Some(&declared_plan_json()), &context).unwrap_err();
        assert!(err.to_string().contains("STATIC_WEB_OUTPUT_MISSING"), "{err}");
        assert_eq!(*transport.prepares.borrow(), 0);
    }

    #[test]
    fn declared_plan_produces_and_uploads() {
        let image = build_image_with_dist();
        let parent = tempfile::tempdir().unwrap();
        let transport = RecordingTransport::default();
        let context = StaticWebEmitContext {
            image_root: image.path(),
            destination_parent: parent.path(),
            runtime_secret_canaries: &[],
            job_id: "job_1",
            build_config_revision_id: "bcrev_1",
            plan_digest: "sha256:abc",
            agent_id: "builder_1",
            transport: &transport,
        };
        let result = emit_static_web_if_declared(Some(&declared_plan_json()), &context).unwrap();
        match result {
            StaticWebEmit::Emitted { materialization_id, manifest_digest } => {
                assert_eq!(materialization_id, "swm_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
                assert!(manifest_digest.starts_with("sha256:"));
            }
            other => panic!("expected Emitted, got {other:?}"),
        }
        assert_eq!(*transport.prepares.borrow(), 1);
        assert_eq!(*transport.completes.borrow(), 1);
        assert!(parent.path().join("static-web-bundle-v1/manifest.json").exists());
    }
}
