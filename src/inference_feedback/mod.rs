mod api;
mod editor;
mod format;
mod payloads;

#[allow(unused_imports)]
pub(crate) use api::{
    request_retry_install_draft, should_collect_feedback, submit_attempt, submit_smoke_failed,
    submit_verified_fix, telemetry_enabled,
};
#[allow(unused_imports)]
pub(crate) use editor::{
    build_manual_manifest_path, can_open_editor_automatically, open_editor, read_manual_manifest,
    write_manual_manifest,
};
#[allow(unused_imports)]
pub(crate) use format::{
    build_manual_intervention_error, build_manual_intervention_message, summarize_manifest_diff,
};
pub(crate) use payloads::InferenceAttemptHandle;

pub(super) const ENV_TELEMETRY: &str = "ATO_TELEMETRY";
pub(super) const MAX_SMOKE_ERROR_EXCERPT_CHARS: usize = 4000;

#[cfg(test)]
mod tests;
