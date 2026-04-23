use crate::config;
use crate::error::{CapsuleError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryPeer {
    pub peer_id: String,
    pub addresses: Vec<String>,
}

#[derive(Debug, Default, Clone)]
pub struct DiscoveryResult {
    pub peers: Vec<DiscoveryPeer>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DiscoveryRegistry {
    #[serde(default)]
    pub peers: Vec<DiscoveryPeer>,
    #[serde(default)]
    pub tags: HashMap<String, Vec<String>>,
}

/// Local discovery PoC implementation.
///
/// Reads static peers from ~/.ato/discovery_peers.json and merges results
/// across mDNS/DHT/GossipSub/Relay backends.
pub fn discover_peers() -> Result<DiscoveryResult> {
    let mut peers = Vec::new();
    merge_peers(&mut peers, discover_peers_mdns()?.peers);
    merge_peers(&mut peers, discover_peers_dht()?.peers);
    merge_peers(&mut peers, discover_peers_gossipsub()?.peers);
    merge_peers(&mut peers, discover_peers_relay()?.peers);

    Ok(DiscoveryResult { peers })
}

pub fn discover_peers_mdns() -> Result<DiscoveryResult> {
    let registry = load_registry()?;
    Ok(DiscoveryResult {
        peers: filter_tagged_peers(&registry, "mdns"),
    })
}

pub fn discover_peers_dht() -> Result<DiscoveryResult> {
    let registry = load_registry()?;
    Ok(DiscoveryResult {
        peers: filter_tagged_peers(&registry, "dht"),
    })
}

pub fn discover_peers_gossipsub() -> Result<DiscoveryResult> {
    let registry = load_registry()?;
    Ok(DiscoveryResult {
        peers: filter_tagged_peers(&registry, "gossipsub"),
    })
}

pub fn discover_peers_relay() -> Result<DiscoveryResult> {
    let registry = load_registry()?;
    Ok(DiscoveryResult {
        peers: filter_tagged_peers(&registry, "relay"),
    })
}

fn merge_peers(into: &mut Vec<DiscoveryPeer>, incoming: Vec<DiscoveryPeer>) {
    for peer in incoming {
        if let Some(existing) = into.iter_mut().find(|p| p.peer_id == peer.peer_id) {
            for addr in peer.addresses {
                if !existing.addresses.contains(&addr) {
                    existing.addresses.push(addr);
                }
            }
        } else {
            into.push(peer);
        }
    }
}

fn filter_tagged_peers(registry: &DiscoveryRegistry, tag: &str) -> Vec<DiscoveryPeer> {
    let mut peers = Vec::new();
    let Some(peer_ids) = registry.tags.get(tag) else {
        return peers;
    };

    for peer_id in peer_ids {
        if let Some(peer) = registry.peers.iter().find(|p| &p.peer_id == peer_id) {
            peers.push(peer.clone());
        }
    }

    peers
}

fn load_registry() -> Result<DiscoveryRegistry> {
    let path = config::config_dir()
        .map_err(|e| CapsuleError::Config(format!("Failed to resolve config dir: {}", e)))?
        .join("discovery_peers.json");
    if !path.exists() {
        return Ok(DiscoveryRegistry::default());
    }

    let raw = fs::read_to_string(&path)
        .map_err(|e| CapsuleError::Config(format!("Failed to read discovery registry: {}", e)))?;
    serde_json::from_str(&raw)
        .map_err(|e| CapsuleError::Config(format!("Failed to parse discovery registry: {}", e)))
}
