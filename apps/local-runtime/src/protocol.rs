//! The runtime's wire protocol.
//!
//! Deliberately generic. A Computation does not know what a feed, a library or
//! a desktop app is, and neither does this: the vocabulary here is `execution`,
//! `project`, `head`, `status` — the same words the execution library uses.
//! Product meaning belongs to whoever calls this, not to the runtime.

use serde::{Deserialize, Serialize};

/// Emitted on stdout, once, when the runtime can actually serve requests.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum Event {
    #[serde(rename = "ato.local-runtime.ready")]
    Ready { port: u16 },
    #[serde(rename = "ato.local-runtime.failed")]
    Failed { reason: String },
}

#[derive(Debug, Deserialize)]
pub struct StartRequest {
    /// Filesystem path of the Capsule project to execute.
    pub project: String,
    #[serde(default)]
    pub bindings: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct ProjectRequest {
    pub project: String,
}

/// A listing of the Computations a runtime holds.
///
/// A wrapper rather than a bare array so the response can grow a cursor or a
/// summary later without becoming a different shape.
#[derive(Debug, Serialize)]
pub struct ExecutionList {
    pub executions: Vec<ExecutionView>,
}

/// What the runtime knows about an execution.
///
/// `head` and `record_seq` are the Computation's own evolution markers, carried
/// straight through from the repository rather than re-interpreted here.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ExecutionView {
    pub execution_id: String,
    pub project: String,
    pub branch: String,
    pub head: String,
    pub record_seq: u64,
    pub status: String,
    /// The supervising worker process, when one is running.
    pub pid: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_is_one_machine_readable_line() {
        let line = serde_json::to_string(&Event::Ready { port: 43128 }).unwrap();
        assert_eq!(line, r#"{"type":"ato.local-runtime.ready","port":43128}"#);
        assert!(!line.contains('\n'));
    }

    #[test]
    fn failure_carries_its_reason() {
        let line = serde_json::to_string(&Event::Failed {
            reason: "work root is not writable".into(),
        })
        .unwrap();
        assert!(line.contains(r#""type":"ato.local-runtime.failed""#));
    }

    #[test]
    fn the_protocol_uses_execution_vocabulary_not_product_vocabulary() {
        // Guard against product words leaking into a runtime protocol: this
        // layer must stay reusable by anything that executes a Computation.
        let view = ExecutionView {
            execution_id: "e1".into(),
            project: "/tmp/p".into(),
            branch: "main".into(),
            head: "blake3:abc".into(),
            record_seq: 0,
            status: "active".into(),
            pid: Some(1),
        };
        let json = serde_json::to_string(&view).unwrap();
        for product_word in ["capsule_card", "feed", "library", "desktop", "activity"] {
            assert!(
                !json.contains(product_word),
                "{product_word} leaked: {json}"
            );
        }
    }
}
