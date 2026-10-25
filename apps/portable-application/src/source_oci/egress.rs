//! Source-to-OCI uses the common netd exact-host gate. No second policy engine.
pub use netd::egress::gate::{EgressAllowance, EgressReport, EgressTarget};
pub struct EgressGate(netd::egress::gate::EgressGate);
impl EgressGate {
    pub fn start(bind: std::net::SocketAddr, allowance: EgressAllowance) -> super::Result<Self> {
        netd::egress::gate::EgressGate::start(bind, allowance)
            .map(Self)
            .map_err(|e| super::err("source_oci_egress_gate_failed", e.to_string()))
    }
    pub fn address(&self) -> std::net::SocketAddr {
        self.0.address()
    }
    pub fn exhausted(&self) -> bool {
        self.0.exhausted()
    }
    pub fn transferred(&self) -> u64 {
        self.0.transferred()
    }
    pub fn report(&self) -> EgressReport {
        self.0.report()
    }
    pub fn stop(self) -> EgressReport {
        self.0.stop()
    }
}
