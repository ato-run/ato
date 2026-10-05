//! Owner-scoped broker assembly. The source receives only a phase-local Unix
//! bridge into netd, never a host network interface or a management credential.
use super::{ExplorationSandbox, ExplorationTicket};
use anyhow::{Result, ensure};
use ato_formation::requirements::{ExecutionPhase, ExecutionRequirements};
use netd::egress::gate::EgressGate;
#[cfg(unix)]
use netd::egress::{gate::EgressAllowance, policy::TransferBudget};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::sync::Arc;

pub(super) struct ScopedGates {
    #[cfg(unix)]
    bridges: Vec<ato_runtime_attempt::network_bridge::HostBridge>,
    gates: BTreeMap<ExecutionPhase, EgressGate>,
    pub sockets: BTreeMap<ExecutionPhase, PathBuf>,
}
impl ScopedGates {
    /// A retained functional Run only resumes the sealed workspace. Its
    /// dependency/build declarations remain checked against the saved owner
    /// grant, but those phases do not execute or receive network authority.
    pub fn retained_runtime_requirements(
        declared: &ExecutionRequirements,
        ceiling: &ExecutionRequirements,
    ) -> Result<ExecutionRequirements> {
        declared.within(ceiling).map_err(|e| anyhow::anyhow!(e.0))?;
        let mut runtime = declared.clone();
        runtime
            .network
            .retain(|n| n.phase == ExecutionPhase::Runtime);
        Ok(runtime)
    }

    pub fn start(
        ticket: &ExplorationTicket,
        configured: Option<&ExplorationSandbox>,
        requirements: &ExecutionRequirements,
        control: &Path,
    ) -> Result<Self> {
        let configured =
            configured.ok_or_else(|| anyhow::anyhow!("exploration_sandbox_unconfigured"))?;
        ticket
            .ceiling
            .within(&configured.ceiling)
            .map_err(|e| anyhow::anyhow!(e.0))?;
        requirements
            .within(&ticket.ceiling)
            .map_err(|e| anyhow::anyhow!(e.0))?;
        // The current physical adapter is HTTPS CONNECT only. A mixed-port
        // host/port Cartesian product must never widen canonical D's scope.
        ensure!(
            requirements.network.iter().all(|r| r.port == 443),
            "exploration_unsupported_network_protocol"
        );
        ensure!(!ticket.search_id.is_empty(), "exploration_search_missing");
        ensure!(
            ticket.network_transfer_bytes <= configured.max_network_transfer_bytes_per_attempt,
            "exploration_network_reservation_exceeded"
        );
        ensure!(
            requirements.network.is_empty() || ticket.network_transfer_bytes > 0,
            "exploration_network_budget_exhausted"
        );
        #[cfg(not(unix))]
        {
            let _ = control;
            anyhow::bail!("runtime_cannot_contain_candidate");
        }
        #[cfg(unix)]
        {
            std::fs::create_dir_all(control)?;
            let budget = Arc::new(TransferBudget::new(ticket.network_transfer_bytes.max(1)));
            let mut assembled = Self {
                bridges: vec![],
                gates: BTreeMap::new(),
                sockets: BTreeMap::new(),
            };
            for phase in [
                ExecutionPhase::Dependencies,
                ExecutionPhase::Build,
                ExecutionPhase::Runtime,
            ] {
                let mut hosts = requirements
                    .network
                    .iter()
                    .filter(|n| n.phase == phase)
                    .map(|n| n.host.clone())
                    .collect::<Vec<_>>();
                let mut ports = requirements
                    .network
                    .iter()
                    .filter(|n| n.phase == phase)
                    .map(|n| n.port)
                    .collect::<Vec<_>>();
                hosts.sort();
                hosts.dedup();
                ports.sort();
                ports.dedup();
                // netd's generic empty policy is permissive. A reserved .invalid
                // sentinel makes an empty phase explicitly deny every target.
                if hosts.is_empty() {
                    hosts.push("exploration-denied.invalid".into());
                    ports.push(443);
                }
                let gate = EgressGate::start_with_budget(
                    "127.0.0.1:0".parse()?,
                    EgressAllowance {
                        hosts,
                        ports,
                        max_transfer_bytes: budget.limit(),
                    },
                    budget.clone(),
                )?;
                let socket = control.join(match phase {
                    ExecutionPhase::Dependencies => "dependencies.sock",
                    ExecutionPhase::Build => "build.sock",
                    ExecutionPhase::Runtime => "runtime.sock",
                });
                let bridge = ato_runtime_attempt::network_bridge::HostBridge::start(
                    &socket,
                    gate.address(),
                )?;
                assembled.sockets.insert(phase, socket);
                assembled.bridges.push(bridge);
                assembled.gates.insert(phase, gate);
            }
            Ok(assembled)
        }
    }
    pub fn evidence(&self) -> serde_json::Value {
        serde_json::json!({"kind":"exploration_network_evidence","reports":self.gates.iter().map(|(phase,gate)|serde_json::json!({"phase":phase,"report":gate.report()})).collect::<Vec<_>>()})
    }

    /// A positive gate observation can stop a refused build promptly. The
    /// bounded observational channel cannot prove the absence of refusals.
    pub fn first_refusal(
        &self,
        phase: ExecutionPhase,
    ) -> Option<ato_formation::failure::FormationFailure> {
        use ato_formation::failure::{FailureStage, FormationFailure};
        let gate = self.gates.get(&phase)?;
        // This authoritative shared counter works even when the bounded
        // observational refusal channel dropped a decision under load.
        if gate.exhausted() {
            return Some(FormationFailure::new(
                "exploration_network_budget_exhausted",
                FailureStage::Build,
                format!(
                    "phase {phase:?} exhausted the frozen attempt transfer budget; build process group stopped"
                ),
            ));
        }
        gate.report()
            .refused
            .into_iter()
            .map(|target| ato_formation::requirements::NetworkRequirement {
                phase,
                host: target.target,
                port: target.port,
            })
            .find(|requirement| {
                ExecutionRequirements {
                    network: vec![requirement.clone()],
                    authority: vec![],
                    host: None,
                }
                .validate()
                .is_ok()
            })
            .map(|requirement| {
                FormationFailure::new(
                    "network_denied",
                    FailureStage::Build,
                    format!(
                        "phase {phase:?} gate refused {}:{}; build process group stopped",
                        requirement.host, requirement.port
                    ),
                )
            })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn retained_resume_keeps_grant_checks_but_does_not_reopen_build_network() {
        use ato_formation::requirements::NetworkRequirement;
        let mut declared = ExecutionRequirements::default();
        declared.network.push(NetworkRequirement {
            phase: ExecutionPhase::Dependencies,
            host: "registry.npmjs.org".into(),
            port: 443,
        });
        assert!(
            ScopedGates::retained_runtime_requirements(&declared, &Default::default()).is_err()
        );
        let runtime = ScopedGates::retained_runtime_requirements(&declared, &declared).unwrap();
        assert!(runtime.network.is_empty());
        assert_eq!(declared.network.len(), 1);
        let mut networked = runtime;
        networked.network.push(NetworkRequirement {
            phase: ExecutionPhase::Runtime,
            host: "registry.npmjs.org".into(),
            port: 443,
        });
        let kept = ScopedGates::retained_runtime_requirements(&networked, &networked).unwrap();
        assert_eq!(kept.network, networked.network);
        let ticket = ExplorationTicket {
            search_id: "functional-retained".into(),
            ceiling: networked.clone(),
            network_transfer_bytes: 0,
            max_retries: 0,
            deadline_ms: None,
        };
        let configured = ExplorationSandbox {
            ceiling: networked,
            max_network_transfer_bytes_per_attempt: 0,
            source_oci: None,
        };
        let denied = ScopedGates::start(
            &ticket,
            Some(&configured),
            &kept,
            Path::new(".tmp/unreached-gate"),
        );
        assert_eq!(
            denied.err().unwrap().to_string(),
            "exploration_network_budget_exhausted"
        );
    }

    #[test]
    fn actual_gate_refusal_is_reported_only_for_its_phase_without_external_dns() {
        let gate = EgressGate::start(
            "127.0.0.1:0".parse().unwrap(),
            EgressAllowance {
                hosts: vec!["exploration-denied.invalid".into()],
                ports: vec![443],
                max_transfer_bytes: 1000,
            },
        )
        .unwrap();
        let mut stream = std::net::TcpStream::connect(gate.address()).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        write!(
            stream,
            "CONNECT registry.npmjs.org:443 HTTP/1.1\r\nHost: registry.npmjs.org:443\r\n\r\n"
        )
        .unwrap();
        let mut response = [0; 512];
        let n = stream.read(&mut response).unwrap();
        assert!(String::from_utf8_lossy(&response[..n]).contains("403"));
        let gates = ScopedGates {
            bridges: vec![],
            sockets: BTreeMap::new(),
            gates: BTreeMap::from([(ExecutionPhase::Dependencies, gate)]),
        };
        let start = std::time::Instant::now();
        let refused = loop {
            if let Some(refused) = gates.first_refusal(ExecutionPhase::Dependencies) {
                break refused;
            }
            assert!(start.elapsed() < std::time::Duration::from_secs(2));
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert_eq!(refused.code, "network_denied");
        assert!(refused.message.contains("registry.npmjs.org:443"));
        assert!(gates.first_refusal(ExecutionPhase::Build).is_none());
    }

    #[test]
    fn shared_transfer_exhaustion_is_distinct_from_an_allowlist_refusal() {
        let budget = Arc::new(TransferBudget::new(1000));
        let gate = EgressGate::start_with_budget(
            "127.0.0.1:0".parse().unwrap(),
            EgressAllowance {
                hosts: vec!["registry.npmjs.org".into()],
                ports: vec![443],
                max_transfer_bytes: 1000,
            },
            budget.clone(),
        )
        .unwrap();
        let gates = ScopedGates {
            bridges: vec![],
            sockets: BTreeMap::new(),
            gates: BTreeMap::from([(ExecutionPhase::Dependencies, gate)]),
        };
        assert!(gates.first_refusal(ExecutionPhase::Dependencies).is_none());
        assert!(!budget.consume(1001));
        assert!(
            gates.gates[&ExecutionPhase::Dependencies]
                .report()
                .refused
                .is_empty()
        );
        let refusal = gates.first_refusal(ExecutionPhase::Dependencies).unwrap();
        assert_eq!(refusal.code, "exploration_network_budget_exhausted");
        assert!(gates.first_refusal(ExecutionPhase::Build).is_none());
    }
}
