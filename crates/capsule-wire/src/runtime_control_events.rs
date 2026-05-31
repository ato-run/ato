use serde::{Deserialize, Serialize};

use crate::placement::{PlacedSessionSummary, PlacementProviderId, PlacementProviderKind};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeControlEvent {
    ProviderStatus {
        provider_id: PlacementProviderId,
        provider_kind: PlacementProviderKind,
        online: bool,
    },
    SessionStarted {
        session: PlacedSessionSummary,
    },
    SessionStopped {
        session_id: String,
        stopped: bool,
    },
    LogLine {
        session_id: String,
        line: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sequence: Option<u64>,
    },
    UrlReady {
        session_id: String,
        user_visible_url: String,
    },
    Error {
        code: String,
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_control_event_round_trips() {
        let original = RuntimeControlEvent::ProviderStatus {
            provider_id: PlacementProviderId::new("desktop:local"),
            provider_kind: PlacementProviderKind::Desktop,
            online: true,
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let parsed: RuntimeControlEvent = serde_json::from_str(&json).expect("parse");
        assert_eq!(parsed, original);
    }

    #[test]
    fn runtime_control_event_tolerates_unknown_fields() {
        let json = r#"{
          "type": "log_line",
          "session_id": "sess_1",
          "line": "ready",
          "unknown": "ignored"
        }"#;
        let parsed: RuntimeControlEvent = serde_json::from_str(json).expect("parse");
        assert_eq!(
            parsed,
            RuntimeControlEvent::LogLine {
                session_id: "sess_1".to_string(),
                line: "ready".to_string(),
                stream: None,
                sequence: None
            }
        );
    }
}
