use std::collections::BTreeMap;

use capsule_protocol::ConnectorId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentMechanism {
    PtyEndpoint,
    HttpProxy,
    TcpProxy,
    UnixSocket,
    NamedPipe,
    WaylandSocket,
    Vsock,
    EnvironmentProjection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentRequirement {
    pub connector_id: ConnectorId,
    pub accepted_mechanisms: Vec<AttachmentMechanism>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentEndpoint {
    pub mechanism: AttachmentMechanism,
    pub address: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AttachmentPlan {
    pub connectors: BTreeMap<ConnectorId, AttachmentEndpoint>,
    pub environment: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateRuntimeCapabilities {
    pub restore_paused: bool,
    pub live_checkpoint: bool,
    pub local_checkpoint: bool,
    pub portable_export: bool,
    pub atomic_snapshot: bool,
    pub attachment_mechanisms: Vec<AttachmentMechanism>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortableEligibility {
    Eligible { contract: String },
    RequiresSanitization { reasons: Vec<String> },
    Ineligible { reasons: Vec<String> },
}

impl AttachmentPlan {
    pub fn resolve(
        requirements: &[AttachmentRequirement],
        available: &[AttachmentMechanism],
    ) -> Result<Self, ConnectorId> {
        let mut plan = Self::default();
        for requirement in requirements {
            let Some(mechanism) = requirement
                .accepted_mechanisms
                .iter()
                .find(|candidate| available.contains(candidate))
            else {
                return Err(requirement.connector_id.clone());
            };
            plan.connectors.insert(
                requirement.connector_id.clone(),
                AttachmentEndpoint {
                    mechanism: mechanism.clone(),
                    address: String::new(),
                },
            );
        }
        Ok(plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_plan_fails_before_restore_when_no_mechanism_matches() {
        let connector_id = ConnectorId::parse("network.main").expect("connector id");
        let requirements = [AttachmentRequirement {
            connector_id: connector_id.clone(),
            accepted_mechanisms: vec![AttachmentMechanism::HttpProxy],
        }];

        let error = AttachmentPlan::resolve(&requirements, &[AttachmentMechanism::PtyEndpoint])
            .expect_err("incompatible attachment must fail");
        assert_eq!(error, connector_id);
    }
}
