use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum LifecycleEvent {
    #[serde(alias = "ipc_ready")]
    Ready {
        service: String,
        #[serde(default)]
        endpoint: Option<String>,
        #[serde(default)]
        port: Option<u16>,
    },
    /// Workload launched but no readiness signal exists (no probe / no port).
    /// Honest "started, not ready" — must NOT be surfaced as ready.
    #[serde(alias = "service_started")]
    Started {
        service: String,
        #[serde(default)]
        endpoint: Option<String>,
        #[serde(default)]
        port: Option<u16>,
    },
    #[serde(alias = "service_exited")]
    Exited {
        service: String,
        #[serde(default)]
        exit_code: Option<i32>,
    },
}

impl LifecycleEvent {
    pub fn service(&self) -> &str {
        match self {
            Self::Ready { service, .. }
            | Self::Started { service, .. }
            | Self::Exited { service, .. } => service,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LifecycleEvent;

    #[test]
    fn parses_nacelle_ipc_ready_as_ready_event() {
        let event: LifecycleEvent = serde_json::from_str(
            r#"{"event":"ipc_ready","service":"main","endpoint":"unix:///tmp/main.sock"}"#,
        )
        .expect("parse ready event");

        assert_eq!(
            event,
            LifecycleEvent::Ready {
                service: "main".to_string(),
                endpoint: Some("unix:///tmp/main.sock".to_string()),
                port: None,
            }
        );
    }

    #[test]
    fn parses_nacelle_service_started_as_started_event() {
        // The honest "started, not ready" signal must parse to Started — never
        // to Ready.
        let event: LifecycleEvent =
            serde_json::from_str(r#"{"event":"service_started","service":"hello-capsule"}"#)
                .expect("parse started event");
        assert_eq!(
            event,
            LifecycleEvent::Started {
                service: "hello-capsule".to_string(),
                endpoint: None,
                port: None,
            }
        );
        assert!(
            !matches!(event, LifecycleEvent::Ready { .. }),
            "service_started must never deserialize as Ready"
        );
    }

    #[test]
    fn parses_nacelle_service_exited_as_exited_event() {
        let event: LifecycleEvent =
            serde_json::from_str(r#"{"event":"service_exited","service":"main","exit_code":42}"#)
                .expect("parse exited event");

        assert_eq!(
            event,
            LifecycleEvent::Exited {
                service: "main".to_string(),
                exit_code: Some(42),
            }
        );
    }
}
