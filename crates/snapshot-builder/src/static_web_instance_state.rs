//! Artifact-native declaration of the Browser Instance State lane.
//!
//! A Static Compute artifact that carries these two elements needs no
//! structural rewriting at serve time: the delivery edge only replaces the
//! placeholder's text with the instance's own state. Without them the edge has
//! to splice the whole lane into the document on every request, which means
//! the bytes a person runs are not the bytes that were built.
//!
//! The bridge SCRIPT is deliberately not embedded here. It is served by the
//! instance host at the reserved path below, so exactly one copy of it exists
//! while this builder still lives on the pre-restructure crate topology.
//! Embedding it belongs with the builder's move onto the current `ato` tree —
//! see `docs/ops/staging-snapshot-builder-sugamo.md`.

/// Reserved path the instance host serves the bridge from.
pub const INSTANCE_STATE_BRIDGE_PATH: &str = "__ato/instance-state-bridge-v1.js";

/// `null` is the artifact's placeholder: the same immutable bytes are served
/// on the anonymous public Static Web lane, where no ComputeInstance exists.
/// The bridge stays completely inert until an edge that resolved an owner
/// replaces this text, so the public lane is unaffected by construction rather
/// than by a flag.
pub const INSTANCE_STATE_PLACEHOLDER: &str =
    r#"<script id="__ato_instance_state_v1" type="application/json">null</script>"#;

/// Parser-blocking and classic on purpose: hydration has to finish before any
/// application script observes `localStorage`.
pub const INSTANCE_STATE_BRIDGE_SCRIPT_TAG: &str =
    r#"<script src="/__ato/instance-state-bridge-v1.js"></script>"#;

/// The placeholder must precede the bridge, and both must precede the App.
pub fn instance_state_head() -> String {
    format!("{INSTANCE_STATE_PLACEHOLDER}{INSTANCE_STATE_BRIDGE_SCRIPT_TAG}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_is_ordered_and_parser_blocking() {
        let head = instance_state_head();
        assert!(head.find("__ato_instance_state_v1").unwrap() < head.find("src=").unwrap());
        assert!(!head.contains("defer"));
        assert!(!head.contains("async"));
        // `null`, not an empty document: the bridge distinguishes "no instance"
        // from "an instance holding nothing".
        assert!(head.contains(">null</script>"));
    }
}
