//! Deterministic immutable Static Web Bundle materialization.
//!
//! This extension owns the physical static-output contract and producer. It
//! has no snapshot provider, deployment, R2, route, or Browser Runner concern.

#![forbid(unsafe_code)]

mod bundle;
mod manifest;
mod output;
mod receipt;

pub use bundle::{
    MAX_DIRECTORY_COUNT, MAX_FILE_COUNT, MAX_FILE_SIZE, MAX_RECURSION_DEPTH, MAX_TOTAL_SIZE,
    ProducedStaticWebBundle, produce_static_web_bundle,
};
pub use manifest::{
    STATIC_WEB_FRAME_ANCESTORS_V1, STATIC_WEB_MANIFEST_V1_SCHEMA, StaticWebFileV1,
    StaticWebManifestError, StaticWebManifestV1, StaticWebRoutingV1, StaticWebSecurityV1,
    canonical_jcs_bytes, canonicalize_connect_sources, is_allowed_media_type,
    validate_connect_source, validate_relative_path,
};
pub use output::{
    ExtractedStaticWebOutput, INSTANCE_STATE_BRIDGE_PATH, INSTANCE_STATE_ELEMENT_ID,
    StaticWebInstrumentation, StaticWebOutputPlan, extract_static_web_output,
    extract_static_web_output_instrumented, extract_static_web_output_with_browser_runner,
};
pub use receipt::{
    STATIC_WEB_BLOB_V1_SCHEMA, STATIC_WEB_BUNDLE_RECEIPT_V1_SCHEMA, StaticWebBlobMetadataV1,
    StaticWebBlobReceiptV1, StaticWebBundleReceiptV1, StaticWebReceiptError, blob_r2_key,
    host_label, manifest_r2_key,
};
