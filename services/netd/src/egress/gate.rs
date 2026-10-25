//! The egress gate of an online source build: netd's CONNECT proxy with an
//! exact hostname allowlist, a port allowlist, denied private and special
//! address ranges (DNS rebinding cannot reach the host or a LAN), and one
//! transfer budget shared by every tunnel. The gate never forwards plain
//! HTTP and never resolves names for a host outside the allowlist.
//!
//! Policy enforcement (allowlists, denied ranges, budget) is authoritative.
//! Recorded decisions are observational evidence only: netd sends them on a
//! bounded channel with `try_send`, so a record can be dropped under load.
//! This record is not a complete audit log.
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::EgressManager;
use super::policy::{EgressPolicy, TransferBudget};
use crate::net::resolver::{Resolver, SystemResolver};
use ato_ipc::net::receipt::{EgressDecision, NetworkEgressDecision};
use serde::Serialize;

use anyhow::{Result, anyhow, ensure};

/// Address ranges a build may never reach through the gate, whatever a
/// public name resolves to.
const DENIED_RANGES: &[&str] = &[
    "0.0.0.0/8",
    "10.0.0.0/8",
    "100.64.0.0/10",
    "127.0.0.0/8",
    "169.254.0.0/16",
    "172.16.0.0/12",
    "192.0.0.0/24",
    "192.0.2.0/24",
    "192.168.0.0/16",
    "198.18.0.0/15",
    "198.51.100.0/24",
    "203.0.113.0/24",
    "224.0.0.0/4",
    "240.0.0.0/4",
    "::/128",
    "::1/128",
    "::ffff:0:0/96",
    "64:ff9b::/96",
    "fc00::/7",
    "fe80::/10",
    "ff00::/8",
];

/// One allowlist: exact lowercase host names, ports and a byte budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EgressAllowance {
    pub hosts: Vec<String>,
    pub ports: Vec<u16>,
    pub max_transfer_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EgressTarget {
    pub target: String,
    pub port: u16,
    pub stage: String,
    pub count: u64,
}

/// What the gate observed while it ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EgressReport {
    pub allowance: EgressAllowance,
    pub transferred_bytes: u64,
    pub budget_exhausted: bool,
    pub allowed: Vec<EgressTarget>,
    pub refused: Vec<EgressTarget>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub evidence_truncated: bool,
}

pub struct EgressGate {
    runtime: Option<tokio::runtime::Runtime>,
    manager: Option<EgressManager>,
    address: SocketAddr,
    budget: Arc<TransferBudget>,
    decisions: Arc<Mutex<Vec<NetworkEgressDecision>>>,
    evidence_truncated: Arc<AtomicBool>,
    allowance: EgressAllowance,
}

impl EgressGate {
    /// Listen on `bind` (port 0 picks one) with exactly `allowance`.
    pub fn start(bind: SocketAddr, allowance: EgressAllowance) -> Result<Self> {
        let budget = Arc::new(TransferBudget::new(allowance.max_transfer_bytes));
        Self::start_with_budget(bind, allowance, budget)
    }

    /// Share a single attempt reservation across dependency/build/runtime gates.
    /// Search-wide accounting is owned by the Coordinator, not this process.
    pub fn start_with_budget(
        bind: SocketAddr,
        allowance: EgressAllowance,
        budget: Arc<TransferBudget>,
    ) -> Result<Self> {
        ensure!(
            !allowance.hosts.is_empty()
                && !allowance.ports.is_empty()
                && allowance.max_transfer_bytes > 0,
            "egress allowance must have explicit hosts, ports and transfer bound"
        );
        ensure!(
            budget.limit() == allowance.max_transfer_bytes,
            "gate reservation mismatch"
        );
        let failed = |d: String| anyhow!("egress_gate_failed: {d}");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| failed(e.to_string()))?;
        let mut policy = EgressPolicy::default().with_transfer_budget(budget.clone());
        for host in &allowance.hosts {
            policy = policy.with_hostname_allow(host.clone());
        }
        for port in &allowance.ports {
            policy = policy.with_port_allow(*port);
        }
        for range in DENIED_RANGES {
            policy = policy.with_cidr_deny(range.parse().expect("static range"));
        }
        let decisions = Arc::new(Mutex::new(Vec::new()));
        let collected = decisions.clone();
        let evidence_truncated = Arc::new(AtomicBool::new(false));
        let truncated = evidence_truncated.clone();
        let (manager, address) = runtime.block_on(async move {
            let resolver: Arc<dyn Resolver + Send + Sync> =
                Arc::new(SystemResolver::new().map_err(|e| failed(format!("resolver: {e}")))?);
            let (sender, mut receiver) = tokio::sync::mpsc::channel(1024);
            tokio::spawn(async move {
                while let Some(decision) = receiver.recv().await {
                    if let Ok(mut all) = collected.lock() {
                        if all.len() < 4096 {
                            all.push(decision);
                        } else {
                            truncated.store(true, Ordering::Release);
                        }
                    }
                }
            });
            let manager = EgressManager::start_on(bind, resolver, Arc::new(policy), sender)
                .await
                .map_err(|e| failed(format!("bind {bind}: {e:#}")))?;
            let address = SocketAddr::new(bind.ip(), manager.port());
            Ok::<_, anyhow::Error>((manager, address))
        })?;
        Ok(Self {
            runtime: Some(runtime),
            manager: Some(manager),
            address,
            budget,
            decisions,
            evidence_truncated,
            allowance,
        })
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn exhausted(&self) -> bool {
        self.budget.exhausted()
    }

    pub fn transferred(&self) -> u64 {
        self.budget.used()
    }

    pub fn report(&self) -> EgressReport {
        let decisions = self.decisions.lock().map(|d| d.clone()).unwrap_or_default();
        let mut allowed = BTreeMap::<(String, u16, String), u64>::new();
        let mut refused = BTreeMap::<(String, u16, String), u64>::new();
        for decision in decisions {
            let key = (decision.target, decision.port, decision.stage);
            let map = if decision.decision == EgressDecision::Allow {
                &mut allowed
            } else {
                &mut refused
            };
            *map.entry(key).or_default() += 1;
        }
        let list = |map: BTreeMap<(String, u16, String), u64>| {
            map.into_iter()
                .map(|((target, port, stage), count)| EgressTarget {
                    target,
                    port,
                    stage,
                    count,
                })
                .collect()
        };
        EgressReport {
            allowance: self.allowance.clone(),
            transferred_bytes: self.budget.used(),
            budget_exhausted: self.budget.exhausted(),
            allowed: list(allowed),
            refused: list(refused),
            evidence_truncated: self.evidence_truncated.load(Ordering::Acquire),
        }
    }

    /// Stop listening and cut every open tunnel.
    pub fn stop(mut self) -> EgressReport {
        self.shutdown();
        self.report()
    }

    fn shutdown(&mut self) {
        if let (Some(runtime), Some(manager)) = (self.runtime.as_ref(), self.manager.take()) {
            runtime.block_on(manager.shutdown());
        }
        if let Some(runtime) = self.runtime.take() {
            // Dropping the runtime cancels the relay tasks of open tunnels.
            runtime.shutdown_timeout(std::time::Duration::from_secs(2));
        }
    }
}

impl Drop for EgressGate {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    use super::*;

    fn connect(gate: &EgressGate, authority: &str) -> String {
        let mut stream = TcpStream::connect(gate.address()).unwrap();
        write!(
            stream,
            "CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n"
        )
        .unwrap();
        let mut response = [0_u8; 256];
        let n = stream.read(&mut response).unwrap();
        String::from_utf8_lossy(&response[..n])
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned()
    }

    #[test]
    fn only_listed_hosts_and_ports_pass_and_private_addresses_never_do() {
        let gate = EgressGate::start(
            "127.0.0.1:0".parse().unwrap(),
            EgressAllowance {
                hosts: vec!["localhost".into()],
                ports: vec![443],
                max_transfer_bytes: 1024,
            },
        )
        .unwrap();
        // Not listed: refused before DNS.
        assert!(connect(&gate, "example.com:443").contains("403"));
        // An IP literal is not a listed name.
        assert!(connect(&gate, "93.184.216.34:443").contains("403"));
        // Listed name, unlisted port.
        assert!(connect(&gate, "localhost:22").contains("403"));
        // Listed name and port, but it resolves to loopback: refused.
        assert!(connect(&gate, "localhost:443").contains("403"));
        // The response and the bounded observational collector are async.
        // Observe this decision before stop cancels the collector's task.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !gate.report().refused.iter().any(|t| t.stage == "cidr") {
            assert!(
                std::time::Instant::now() < deadline,
                "CIDR decision was not collected"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let report = gate.stop();
        let stages: Vec<_> = report.refused.iter().map(|t| t.stage.as_str()).collect();
        assert!(stages.contains(&"hostname") && stages.contains(&"port"));
        assert!(stages.contains(&"cidr"), "{stages:?}");
        assert!(report.allowed.is_empty());
        assert_eq!(report.transferred_bytes, 0);
    }
}
