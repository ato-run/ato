//! DAG integration for IPC dependency ordering.
//!
//! IPC imports are added to the existing service DAG as `_ipc_*` nodes.
//! This ensures proper startup ordering: services that a capsule depends on
//! via IPC are started before the capsule itself.
//!
//! ## Reserved Prefixes
//!
//! - `_ipc_` — IPC service nodes (added by this module)
//! - `_setup` — Setup/init scripts
//! - `_main` — Main capsule entrypoint

use std::collections::{HashMap, HashSet};

use thiserror::Error;
use tracing::debug;

use super::types::IpcImportConfig;

/// IPC DAG prefix for generated node names.
pub const IPC_NODE_PREFIX: &str = "_ipc_";

/// Reserved node prefixes that user services must not use.
pub const RESERVED_PREFIXES: &[&str] = &["_ipc_", "_setup", "_main"];

/// DAG error types.
#[derive(Debug, Error)]
pub enum DagError {
    /// Circular dependency detected.
    #[error("Circular dependency detected: {cycle}")]
    CyclicDependency { cycle: String },
    /// Reserved prefix collision.
    #[error("Service name '{name}' uses reserved prefix '{prefix}'")]
    ReservedPrefix { name: String, prefix: String },
    /// Duplicate node.
    #[error("Duplicate DAG node: '{name}'")]
    DuplicateNode { name: String },
}

/// A node in the IPC dependency DAG.
#[derive(Debug, Clone)]
pub struct DagNode {
    /// Node name (e.g., "_ipc_greeter" or "web-frontend").
    pub name: String,
    /// Dependencies (names of nodes that must start first).
    pub depends_on: Vec<String>,
    /// Whether this is an IPC service node (vs. a user-defined service).
    #[allow(dead_code)]
    pub is_ipc: bool,
}

/// Build an IPC-augmented DAG from imports and existing service nodes.
///
/// # Arguments
///
/// * `imports` — IPC import configurations from `[ipc.imports]`.
/// * `existing_services` — Names of already-defined services (from `[services]`).
/// * `main_node` — Name of the main capsule node.
///
/// # Returns
///
/// A list of `DagNode`s representing the startup order, including
/// injected `_ipc_*` nodes for eager imports.
///
/// # Errors
///
/// Returns `DagError::ReservedPrefix` if any existing service name uses a
/// reserved prefix. Returns `DagError::CyclicDependency` if the resulting
/// DAG contains cycles.
pub fn build_ipc_dag(
    imports: &HashMap<String, IpcImportConfig>,
    existing_services: &[String],
    main_node: &str,
) -> Result<Vec<DagNode>, DagError> {
    // Check for reserved prefix collisions in existing services
    for svc in existing_services {
        for prefix in RESERVED_PREFIXES {
            if svc.starts_with(prefix) {
                return Err(DagError::ReservedPrefix {
                    name: svc.clone(),
                    prefix: prefix.to_string(),
                });
            }
        }
    }

    let mut nodes: Vec<DagNode> = Vec::new();
    let mut node_names: HashSet<String> = HashSet::new();

    // Add existing services as DAG nodes
    for svc in existing_services {
        node_names.insert(svc.clone());
        nodes.push(DagNode {
            name: svc.clone(),
            depends_on: vec![],
            is_ipc: false,
        });
    }

    // Add IPC import nodes
    let mut main_deps: Vec<String> = Vec::new();

    for (import_name, config) in imports {
        let ipc_node_name = format!("{}{}", IPC_NODE_PREFIX, import_name);

        if node_names.contains(&ipc_node_name) {
            return Err(DagError::DuplicateNode {
                name: ipc_node_name,
            });
        }

        node_names.insert(ipc_node_name.clone());
        nodes.push(DagNode {
            name: ipc_node_name.clone(),
            depends_on: vec![],
            is_ipc: true,
        });

        // Eager imports are dependencies of the main node
        if config.activation == super::types::ActivationMode::Eager {
            main_deps.push(ipc_node_name);
        }

        debug!(
            import = import_name,
            from = %config.from,
            activation = ?config.activation,
            "Added IPC dependency node"
        );
    }

    // Add/update main node with IPC dependencies
    if !main_node.is_empty() {
        nodes.push(DagNode {
            name: main_node.to_string(),
            depends_on: main_deps,
            is_ipc: false,
        });
        node_names.insert(main_node.to_string());
    }

    // Cycle detection via topological sort
    detect_cycles(&nodes)?;

    Ok(nodes)
}

/// Topological sort with cycle detection (Kahn's algorithm).
fn detect_cycles(nodes: &[DagNode]) -> Result<Vec<String>, DagError> {
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();

    for node in nodes {
        in_degree.entry(&node.name).or_insert(0);
        adj.entry(&node.name).or_default();
    }

    for node in nodes {
        for dep in &node.depends_on {
            adj.entry(dep.as_str()).or_default().push(&node.name);
            *in_degree.entry(&node.name).or_insert(0) += 1;
        }
    }

    let mut queue: Vec<&str> = in_degree
        .iter()
        .filter(|(_, &deg)| deg == 0)
        .map(|(&name, _)| name)
        .collect();

    let mut sorted: Vec<String> = Vec::new();

    while let Some(node) = queue.pop() {
        sorted.push(node.to_string());
        if let Some(dependents) = adj.get(node) {
            for dep in dependents {
                if let Some(deg) = in_degree.get_mut(dep) {
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push(dep);
                    }
                }
            }
        }
    }

    if sorted.len() != nodes.len() {
        // Find cycle participants
        let sorted_set: HashSet<&str> = sorted.iter().map(|s| s.as_str()).collect();
        let cycle_nodes: Vec<String> = nodes
            .iter()
            .filter(|n| !sorted_set.contains(n.name.as_str()))
            .map(|n| n.name.clone())
            .collect();
        return Err(DagError::CyclicDependency {
            cycle: cycle_nodes.join(" → "),
        });
    }

    Ok(sorted)
}

/// Compute the startup order (topological sort).
#[allow(dead_code)]
pub fn startup_order(nodes: &[DagNode]) -> Result<Vec<String>, DagError> {
    detect_cycles(nodes)
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::types::ActivationMode;

    #[test]
    fn test_build_ipc_dag_basic() {
        let mut imports = HashMap::new();
        imports.insert(
            "greeter".to_string(),
            IpcImportConfig {
                from: "greeter-service".to_string(),
                activation: ActivationMode::Eager,
                optional: false,
            },
        );

        let nodes = build_ipc_dag(&imports, &[], "_main").unwrap();

        // Should have _ipc_greeter and _main
        assert_eq!(nodes.len(), 2);

        let main = nodes.iter().find(|n| n.name == "_main").unwrap();
        assert!(main.depends_on.contains(&"_ipc_greeter".to_string()));
    }

    #[test]
    fn test_build_ipc_dag_lazy_not_dependency() {
        let mut imports = HashMap::new();
        imports.insert(
            "analytics".to_string(),
            IpcImportConfig {
                from: "analytics-service".to_string(),
                activation: ActivationMode::Lazy,
                optional: false,
            },
        );

        let nodes = build_ipc_dag(&imports, &[], "_main").unwrap();
        let main = nodes.iter().find(|n| n.name == "_main").unwrap();
        assert!(
            main.depends_on.is_empty(),
            "Lazy imports should not be startup dependencies"
        );
    }

    #[test]
    fn test_build_ipc_dag_reserved_prefix_collision() {
        let services = vec!["_ipc_custom".to_string()];
        let result = build_ipc_dag(&HashMap::new(), &services, "_main");
        assert!(result.is_err());
        match result.unwrap_err() {
            DagError::ReservedPrefix { name, prefix } => {
                assert_eq!(name, "_ipc_custom");
                assert_eq!(prefix, "_ipc_");
            }
            other => panic!("Expected ReservedPrefix, got: {:?}", other),
        }
    }

    #[test]
    fn test_build_ipc_dag_with_existing_services() {
        let mut imports = HashMap::new();
        imports.insert(
            "db".to_string(),
            IpcImportConfig {
                from: "db-service".to_string(),
                activation: ActivationMode::Eager,
                optional: false,
            },
        );

        let services = vec!["web-frontend".to_string()];
        let nodes = build_ipc_dag(&imports, &services, "_main").unwrap();

        // Should have: web-frontend, _ipc_db, _main
        assert_eq!(nodes.len(), 3);
        assert!(nodes.iter().any(|n| n.name == "web-frontend"));
        assert!(nodes.iter().any(|n| n.name == "_ipc_db" && n.is_ipc));
    }

    #[test]
    fn test_cycle_detection_no_cycle() {
        let nodes = vec![
            DagNode {
                name: "a".to_string(),
                depends_on: vec![],
                is_ipc: false,
            },
            DagNode {
                name: "b".to_string(),
                depends_on: vec!["a".to_string()],
                is_ipc: false,
            },
            DagNode {
                name: "c".to_string(),
                depends_on: vec!["b".to_string()],
                is_ipc: false,
            },
        ];

        let order = startup_order(&nodes).unwrap();
        assert_eq!(order.len(), 3);

        // a should come before b, b before c
        let pos_a = order.iter().position(|n| n == "a").unwrap();
        let pos_b = order.iter().position(|n| n == "b").unwrap();
        let pos_c = order.iter().position(|n| n == "c").unwrap();
        assert!(pos_a < pos_b);
        assert!(pos_b < pos_c);
    }

    #[test]
    fn test_cycle_detection_with_cycle() {
        let nodes = vec![
            DagNode {
                name: "a".to_string(),
                depends_on: vec!["c".to_string()],
                is_ipc: false,
            },
            DagNode {
                name: "b".to_string(),
                depends_on: vec!["a".to_string()],
                is_ipc: false,
            },
            DagNode {
                name: "c".to_string(),
                depends_on: vec!["b".to_string()],
                is_ipc: false,
            },
        ];

        let result = startup_order(&nodes);
        assert!(result.is_err());
        match result.unwrap_err() {
            DagError::CyclicDependency { cycle } => {
                assert!(!cycle.is_empty());
            }
            other => panic!("Expected CyclicDependency, got: {:?}", other),
        }
    }

    #[test]
    fn test_empty_dag() {
        let nodes = build_ipc_dag(&HashMap::new(), &[], "").unwrap();
        assert!(nodes.is_empty());
    }
}
