//! Runtime Control API wire types — the loopback HTTP contract between a host
//! (the desktop shell today) and the `ato serve` Runtime Control API.
//!
//! `LaunchSessionRequest` is single-sourced here: it was duplicated between the
//! desktop consumer (`desktop::runtime_control_client`) and the CLI producer
//! (`cli`'s `serve::runtime_api`). The *response* types stay split for now — the
//! producer emits a full session descriptor while the desktop consumer reads a
//! deliberately-minimal view — and fold in with the session-descriptor work in a
//! later redistribution step. Runtime-control *event* DTOs live in the sibling
//! [`crate::runtime_control_events`] module.

use serde::{Deserialize, Serialize};

/// Body of `POST /v1/runtime/sessions` — launch a session for an installed
/// profile. `target_label` maps to `--target` (e.g. `"web"`, `"worker"`); it is
/// omitted on the wire when absent and defaults to `None` when missing, so the
/// merged type matches both the producer and consumer's original behaviour.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchSessionRequest {
    pub install_profile_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_label: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialises_without_target_label() {
        let req = LaunchSessionRequest {
            install_profile_key: "github.com/foo/bar@default".to_string(),
            target_label: None,
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["install_profile_key"], "github.com/foo/bar@default");
        assert!(v.get("target_label").is_none());
    }

    #[test]
    fn serialises_with_target_label() {
        let req = LaunchSessionRequest {
            install_profile_key: "github.com/foo/bar@default".to_string(),
            target_label: Some("gpu".to_string()),
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["target_label"], "gpu");
    }
}
