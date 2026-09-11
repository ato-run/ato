//! A failure a person can act on, carried as a type rather than as prose.
//!
//! Every error in this crate already knows its own `code()`. That knowledge
//! used to die at the worker boundary: `job.rs` flattened each one with
//! `anyhow!("{error}")`, so by the time the worker reported the failure all it
//! had was a sentence. The reporting code then had two bad options — ship the
//! whole `{error:#}` chain to whoever uploaded the source, or replace it with
//! one fixed line — and both were tried.
//!
//! Shipping the chain leaks: a build's error text can carry a filesystem path,
//! a token a tool echoed back, or a dependency URL with a credential in it.
//! Replacing it with a fixed line throws away the only part that says what to
//! change. Re-deriving a code by matching on the prose is worse than either,
//! because it silently breaks the first time somebody improves a message.
//!
//! So the type survives instead. A failure carries its `code` (stable, what a
//! client branches on), its `stage` (where it stopped, so "your manifest is in
//! the wrong grammar" is distinguishable from "your build script exited 1"),
//! and a `message` written for the person who uploaded the source.
//!
//! It implements `std::error::Error`, so it travels inside `anyhow::Error`
//! untouched and the reporter recovers it with `downcast_ref`. Anything that
//! is NOT one of these stays anonymous on purpose: the reporter says so with a
//! generic sentence and keeps the detail in the operator log.

use std::fmt;

use crate::authoring::AuthoringError;
use crate::capsule_toml::CapsuleTomlError;
use crate::intent::IntentError;
use crate::preset::PresetMismatch;
use crate::projection::ProjectionError;

/// Where a Formation job stopped.
///
/// Deliberately coarse. This answers "which part of the pipeline refused it",
/// which is the question that decides who fixes what — not a step index, which
/// would turn into a contract nobody meant to make.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureStage {
    /// Reading and validating what the author wrote.
    Authoring,
    /// Choosing a Preset when the author wrote nothing.
    Preset,
    /// Turning a bound draft into something executable.
    Projection,
    /// Running the build itself.
    Build,
    /// Checking the result satisfies the Contract it claims.
    Verification,
}

impl FailureStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Authoring => "authoring",
            Self::Preset => "preset",
            Self::Projection => "projection",
            Self::Build => "build",
            Self::Verification => "verification",
        }
    }
}

impl fmt::Display for FailureStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A failure with a code, a stage, and a sentence for the uploader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormationFailure {
    pub code: String,
    pub stage: FailureStage,
    pub message: String,
}

impl FormationFailure {
    pub fn new(code: impl Into<String>, stage: FailureStage, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            stage,
            message: message.into(),
        }
    }

    /// The message, bounded to `limit` BYTES — the ellipsis included.
    ///
    /// Bytes, not characters, because the bound exists for the transport and
    /// the store on the far side; a limit that counted characters would let a
    /// Japanese message through at three times the size it claimed.
    ///
    /// Bounded HERE rather than at the reporter, so the bound is part of what a
    /// failure IS and no caller can forget it. Truncation is marked, because a
    /// sentence that stops mid-word without saying so reads as corruption. The
    /// cut walks back to a character boundary: slicing a multi-byte character
    /// in half would panic, and this runs on whatever an author wrote.
    pub fn bounded_message(&self, limit: usize) -> String {
        const MARK: &str = "…";
        if self.message.len() <= limit {
            return self.message.clone();
        }
        let Some(mut cut) = limit.checked_sub(MARK.len()) else {
            return String::new();
        };
        while cut > 0 && !self.message.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}{MARK}", &self.message[..cut])
    }
}

impl fmt::Display for FormationFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for FormationFailure {}

// ─── the conversions ────────────────────────────────────────────────────────
//
// Each one is written at the place the type is still known. That is the whole
// mechanism: nothing downstream has to recognise a message, so improving a
// message never changes a code.

impl From<CapsuleTomlError> for FormationFailure {
    fn from(error: CapsuleTomlError) -> Self {
        Self::new(error.code(), FailureStage::Authoring, error.to_string())
    }
}

impl From<AuthoringError> for FormationFailure {
    fn from(error: AuthoringError) -> Self {
        Self::new(error.code(), FailureStage::Authoring, error.to_string())
    }
}

impl From<IntentError> for FormationFailure {
    fn from(error: IntentError) -> Self {
        Self::new(error.code(), FailureStage::Authoring, error.to_string())
    }
}

impl From<ProjectionError> for FormationFailure {
    fn from(error: ProjectionError) -> Self {
        Self::new(error.code(), FailureStage::Projection, error.to_string())
    }
}

impl From<PresetMismatch> for FormationFailure {
    fn from(mismatch: PresetMismatch) -> Self {
        // The mismatch message is already written for the person who uploaded
        // the source — it is the whole value of the preset layer, and it used
        // to be discarded before anyone could read it.
        Self::new(mismatch.code, FailureStage::Preset, mismatch.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_store_manifest_keeps_its_own_code_and_names_the_authoring_stage() {
        let failure: FormationFailure = CapsuleTomlError::LegacyStoreManifest.into();
        assert_eq!(failure.code, "legacy_store_manifest");
        assert_eq!(failure.stage, FailureStage::Authoring);
        // The sentence is the one the parser wrote, not a re-description.
        assert!(failure.message.contains("store submission manifest"));
    }

    #[test]
    fn a_preset_mismatch_reaches_the_uploader_with_its_own_words() {
        let mismatch = PresetMismatch {
            code: "preset_no_match",
            message: "Ato can turn an HTML file, a folder with an index.html, \
                      or a project that builds to `dist/` into an App."
                .to_owned(),
        };
        let failure: FormationFailure = mismatch.into();
        assert_eq!(failure.code, "preset_no_match");
        assert_eq!(failure.stage, FailureStage::Preset);
        assert!(failure.message.contains("index.html"));
    }

    #[test]
    fn the_message_is_bounded_and_says_that_it_was_cut() {
        let failure = FormationFailure::new("x", FailureStage::Build, "y".repeat(500));
        let bounded = failure.bounded_message(400);
        assert!(bounded.len() <= 400);
        assert!(bounded.ends_with('…'));
    }

    #[test]
    fn bounding_never_splits_a_character() {
        // A multi-byte message cut by bytes would produce invalid UTF-8 and
        // panic; the cut walks back to a boundary instead. The result still
        // honours the byte bound, ellipsis included.
        let failure = FormationFailure::new("x", FailureStage::Build, "あ".repeat(300));
        let bounded = failure.bounded_message(400);
        assert!(bounded.ends_with('…'));
        assert!(bounded.len() <= 400, "{} bytes", bounded.len());
        // And it is still valid UTF-8 that ends on a whole character.
        assert!(bounded.trim_end_matches('…').chars().all(|c| c == 'あ'));
    }

    #[test]
    fn a_short_message_is_left_exactly_as_written() {
        let failure = FormationFailure::new("x", FailureStage::Build, "short");
        assert_eq!(failure.bounded_message(400), "short");
    }

    #[test]
    fn it_survives_a_trip_through_anyhow() {
        // The property the worker depends on: the type is recoverable after
        // the error has been through the generic error channel.
        let failure: FormationFailure = CapsuleTomlError::LegacyStoreManifest.into();
        let erased: anyhow::Error = failure.into();
        let recovered = erased
            .downcast_ref::<FormationFailure>()
            .expect("the typed failure survives");
        assert_eq!(recovered.code, "legacy_store_manifest");
        assert_eq!(recovered.stage, FailureStage::Authoring);
    }
}
