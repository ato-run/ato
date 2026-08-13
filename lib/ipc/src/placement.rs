use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PlacementProviderId(String);

impl PlacementProviderId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlacementProviderKind {
    Desktop,
    Managed,
    External,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PlacementCapabilities {
    #[serde(default)]
    pub supports_launch: bool,
    #[serde(default)]
    pub supports_stop: bool,
    #[serde(default)]
    pub supports_logs: bool,
    #[serde(default)]
    pub supports_open_url: bool,
    #[serde(default)]
    pub supports_start_serve: bool,
    #[serde(default)]
    pub supports_add_capsule: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub isolation_classes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub storage_classes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network_classes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacementFacets {
    pub provider_kind: PlacementProviderKind,
    pub isolation_class: String,
    pub storage_class: String,
    pub network_class: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacementIdentity {
    pub placement_provider: PlacementProviderKind,
    pub placement_provider_id: PlacementProviderId,
    pub placement_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement_facets: Option<PlacementFacets>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacedSessionSummary {
    pub session_id: String,
    pub status: String,
    pub placement: PlacementIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_visible_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_by_client: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_profile_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_profile_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desktop_identity() -> PlacementIdentity {
        PlacementIdentity {
            placement_provider: PlacementProviderKind::Desktop,
            placement_provider_id: PlacementProviderId::new("desktop:local"),
            placement_id: "plc_local_desktop".to_string(),
            placement_fingerprint: Some("sha256:abc".to_string()),
            placement_facets: Some(PlacementFacets {
                provider_kind: PlacementProviderKind::Desktop,
                isolation_class: "local".to_string(),
                storage_class: "local".to_string(),
                network_class: "loopback".to_string(),
                runner_version: Some("0.7.0-dev".to_string()),
            }),
        }
    }

    #[test]
    fn placement_identity_round_trips() {
        let original = desktop_identity();
        let json = serde_json::to_string(&original).expect("serialize");
        let parsed: PlacementIdentity = serde_json::from_str(&json).expect("parse");
        assert_eq!(parsed, original);
    }

    #[test]
    fn placed_session_summary_tolerates_unknown_fields() {
        let json = r#"{
          "session_id": "sess_1",
          "status": "running",
          "placement": {
            "placement_provider": "desktop",
            "placement_provider_id": "desktop:local",
            "placement_id": "plc_local_desktop",
            "future_field": "ignored"
          },
          "future_top_level": true
        }"#;
        let parsed: PlacedSessionSummary = serde_json::from_str(json).expect("parse");
        assert_eq!(parsed.session_id, "sess_1");
        assert_eq!(
            parsed.placement.placement_provider,
            PlacementProviderKind::Desktop
        );
        assert!(parsed.user_visible_url.is_none());
    }
}
