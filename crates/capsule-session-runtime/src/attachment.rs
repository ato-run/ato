use std::collections::BTreeMap;

use capsule_protocol::ConnectorId;
use serde::{Deserialize, Serialize};
use thiserror::Error;

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
    ) -> Result<Self, AttachmentPlanError> {
        let mut plan = Self::default();
        for requirement in requirements {
            if plan.connectors.contains_key(&requirement.connector_id) {
                return Err(AttachmentPlanError::DuplicateConnector(
                    requirement.connector_id.clone(),
                ));
            }
            let Some(mechanism) = requirement
                .accepted_mechanisms
                .iter()
                .find(|candidate| available.contains(candidate))
            else {
                return Err(AttachmentPlanError::Unavailable(
                    requirement.connector_id.clone(),
                ));
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

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AttachmentPlanError {
    #[error("no compatible attachment for Connector {0}")]
    Unavailable(ConnectorId),
    #[error("duplicate active Connector {0}")]
    DuplicateConnector(ConnectorId),
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
        assert_eq!(error, AttachmentPlanError::Unavailable(connector_id));
    }

    #[test]
    fn attachment_plan_rejects_duplicate_connector_ids() {
        let connector_id = ConnectorId::parse("network.main").expect("connector id");
        let requirement = AttachmentRequirement {
            connector_id: connector_id.clone(),
            accepted_mechanisms: vec![AttachmentMechanism::HttpProxy],
        };

        assert_eq!(
            AttachmentPlan::resolve(
                &[requirement.clone(), requirement],
                &[AttachmentMechanism::HttpProxy]
            ),
            Err(AttachmentPlanError::DuplicateConnector(connector_id))
        );
    }
}
