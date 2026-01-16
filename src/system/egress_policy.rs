use crate::capsule_types::capsule_v1::{CapsuleManifestV1, EgressIdType};

/// Generate minimal iptables rules for L3 egress enforcement.
///
/// Note: Full policy resolution is owned by capsule-cli. This is a compatibility
/// shim for legacy tests.
pub fn generate_fw_rules(manifest: &CapsuleManifestV1) -> Vec<String> {
    let mut rules = Vec::new();

    // Default drop (fail-closed)
    rules.push("iptables -P OUTPUT DROP".to_string());
    // Allow loopback
    rules.push("iptables -A OUTPUT -o lo -j ACCEPT".to_string());
    // Allow established/related
    rules.push(
        "iptables -A OUTPUT -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT".to_string(),
    );

    if let Some(network) = &manifest.network {
        for rule in &network.egress_id_allow {
            match rule.rule_type {
                EgressIdType::Ip | EgressIdType::Cidr => {
                    rules.push(format!("iptables -A OUTPUT -d {} -j ACCEPT", rule.value));
                }
                EgressIdType::Spiffe => {
                    // SPIFFE resolution is handled in higher layers; ignore here.
                }
            }
        }
    }

    rules
}
