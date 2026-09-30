//! Owner-scoped broker assembly. The source receives only a phase-local Unix
//! bridge into netd, never a host network interface or a management credential.
use super::{ExplorationSandbox, ExplorationTicket};
use anyhow::{Result, ensure};
use ato_formation::requirements::{ExecutionPhase, ExecutionRequirements};
use netd::egress::{
    gate::{EgressAllowance, EgressGate},
    policy::TransferBudget,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

pub(super) struct ScopedGates {
    #[cfg(unix)]
    bridges: Vec<ato_runtime_attempt::network_bridge::HostBridge>,
    gates: BTreeMap<ExecutionPhase, EgressGate>,
    pub sockets: BTreeMap<ExecutionPhase, PathBuf>,
}
impl ScopedGates {
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
}
