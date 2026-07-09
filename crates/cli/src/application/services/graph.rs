use std::collections::{BTreeSet, HashMap, VecDeque};

use anyhow::Result;
use serde_json::json;

use capsule::execution_plan::error::{
    AtoErrorClassification, AtoExecutionError, ManifestSuggestion,
};
use capsule::router::ManifestData;
use capsule::types::{OrchestrationPlan, ServiceSpec};

#[derive(Debug, Clone)]
pub(crate) struct ServiceGraphPlan {
    services: HashMap<String, ServiceSpec>,
    startup_order: Vec<String>,
    layers: Vec<Vec<String>>,
}

impl ServiceGraphPlan {
    pub(crate) fn from_manifest(plan: &ManifestData) -> Result<Self> {
        let services = plan.services();
        if services.is_empty() {
            return Err(AtoExecutionError::execution_contract_invalid(
                "top-level [services] must define at least one service",
                Some("services"),
                None,
            )
            .with_classification(AtoErrorClassification::Manifest)
            .into());
        }
        if !services.contains_key("main") {
            return Err(AtoExecutionError::execution_contract_invalid(
                "web/deno services mode requires top-level [services.main]",
                Some("services.main"),
                Some("main"),
            )
            .with_classification(AtoErrorClassification::Manifest)
            .with_manifest_suggestion(ManifestSuggestion {
                kind: "create_table".to_string(),
                path: "services.main".to_string(),
                operation: "create_table".to_string(),
                value: Some(json!({})),
                message: "Add a [services.main] entry for web services mode".to_string(),
            })
            .into());
        }

        Self::from_services(&services)
    }

    pub(crate) fn from_services(services: &HashMap<String, ServiceSpec>) -> Result<Self> {
        let startup_order = topo_sort(services)?;
        let layers = build_layers(services)?;
        Ok(Self {
            services: services.clone(),
            startup_order,
            layers,
        })
    }

    /// Build the start-order graph from a resolved [`OrchestrationPlan`] so the
    /// layered scheduler considers target-level `depends_on` (merged into
    /// `ResolvedService.depends_on` by the router) and any cross-service
    /// dependency edges materialized as `ResolvedService.connections`.
    ///
    /// `from_services` looks only at the raw `[services.*]` table and misses
    /// dependencies declared on `[targets.*]`, which is how AFFiNE / Dify /
    /// most multi-service recipes wire their compose-shaped graphs. Use this
    /// constructor whenever the caller already has the resolved plan handy.
    pub(crate) fn from_orchestration(orchestration: &OrchestrationPlan) -> Result<Self> {
        let mut services: HashMap<String, ServiceSpec> = HashMap::new();
        for resolved in &orchestration.services {
            let mut deps: Vec<String> = resolved.depends_on.clone();
            for connection in &resolved.connections {
                if !deps.contains(&connection.dependency) {
                    deps.push(connection.dependency.clone());
                }
            }
            let depends_on = if deps.is_empty() { None } else { Some(deps) };
            services.insert(
                resolved.name.clone(),
                ServiceSpec {
                    depends_on,
                    readiness_probe: resolved.readiness_probe.clone(),
                    ..ServiceSpec::default()
                },
            );
        }
        Self::from_services(&services)
    }

    pub(crate) fn services(&self) -> &HashMap<String, ServiceSpec> {
        &self.services
    }

    pub(crate) fn startup_order(&self) -> &[String] {
        &self.startup_order
    }

    #[allow(dead_code)]
    pub(crate) fn layers(&self) -> &[Vec<String>] {
        &self.layers
    }
}

fn topo_sort(services: &HashMap<String, ServiceSpec>) -> Result<Vec<String>> {
    fn visit(
        current: &str,
        services: &HashMap<String, ServiceSpec>,
        visited: &mut BTreeSet<String>,
        visiting: &mut BTreeSet<String>,
        stack: &mut Vec<String>,
        out: &mut Vec<String>,
    ) -> Result<()> {
        if visited.contains(current) {
            return Ok(());
        }
        if visiting.contains(current) {
            stack.push(current.to_string());
            return Err(AtoExecutionError::execution_contract_invalid(
                format!("services has circular dependency: {}", stack.join(" -> ")),
                Some("services"),
                None,
            )
            .with_classification(AtoErrorClassification::Manifest)
            .into());
        }

        let spec = services.get(current).ok_or_else(|| {
            AtoExecutionError::execution_contract_invalid(
                format!("unknown service '{}' in dependency graph", current),
                Some("services"),
                Some(current),
            )
            .with_classification(AtoErrorClassification::Manifest)
        })?;

        visiting.insert(current.to_string());
        stack.push(current.to_string());
        if let Some(deps) = spec.depends_on.as_ref() {
            for dep in deps {
                if !services.contains_key(dep) {
                    return Err(AtoExecutionError::execution_contract_invalid(
                        format!(
                            "services.{}.depends_on references unknown service '{}'",
                            current, dep
                        ),
                        Some(&format!("services.{}.depends_on", current)),
                        Some(current),
                    )
                    .with_classification(AtoErrorClassification::Manifest)
                    .into());
                }
                visit(dep, services, visited, visiting, stack, out)?;
            }
        }
        stack.pop();
        visiting.remove(current);
        visited.insert(current.to_string());
        out.push(current.to_string());
        Ok(())
    }

    let mut names: Vec<&String> = services.keys().collect();
    names.sort();

    let mut out = Vec::new();
    let mut visited = BTreeSet::new();
    let mut visiting = BTreeSet::new();
    for name in names {
        let mut stack = Vec::new();
        visit(
            name,
            services,
            &mut visited,
            &mut visiting,
            &mut stack,
            &mut out,
        )?;
    }
    Ok(out)
}

fn build_layers(services: &HashMap<String, ServiceSpec>) -> Result<Vec<Vec<String>>> {
    let mut indegree: HashMap<String, usize> =
        services.keys().map(|name| (name.clone(), 0)).collect();
    let mut reverse_edges: HashMap<String, Vec<String>> = HashMap::new();

    for (name, service) in services {
        if let Some(deps) = service.depends_on.as_ref() {
            for dep in deps {
                let Some(count) = indegree.get_mut(name) else {
                    continue;
                };
                *count += 1;
                reverse_edges
                    .entry(dep.clone())
                    .or_default()
                    .push(name.clone());
            }
        }
    }

    let mut ready: VecDeque<String> = indegree
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(name, _)| name.clone())
        .collect();
    let mut ready_sorted: Vec<_> = ready.drain(..).collect();
    ready_sorted.sort();
    ready = ready_sorted.into();

    let mut layers = Vec::new();
    let mut processed = 0usize;
    while !ready.is_empty() {
        let mut layer = Vec::new();
        let mut next_ready = Vec::new();
        let current_width = ready.len();
        for _ in 0..current_width {
            let Some(name) = ready.pop_front() else {
                break;
            };
            processed += 1;
            layer.push(name.clone());
            if let Some(children) = reverse_edges.get(&name) {
                for child in children {
                    let entry = indegree
                        .get_mut(child)
                        .expect("child service must have indegree entry");
                    *entry -= 1;
                    if *entry == 0 {
                        next_ready.push(child.clone());
                    }
                }
            }
        }
        next_ready.sort();
        for name in next_ready {
            ready.push_back(name);
        }
        layer.sort();
        layers.push(layer);
    }

    if processed != services.len() {
        return Err(AtoExecutionError::execution_contract_invalid(
            "services graph could not be layered because dependency validation failed",
            Some("services"),
            None,
        )
        .with_classification(AtoErrorClassification::Manifest)
        .into());
    }

    Ok(layers)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::ServiceGraphPlan;
    use capsule::types::{
        OrchestrationPlan, ResolvedService, ResolvedServiceNetwork, ResolvedServiceRuntime,
        ResolvedTargetRuntime, ServiceConnectionInfo, ServiceSpec,
    };

    fn service(entrypoint: &str, depends_on: Option<Vec<&str>>) -> ServiceSpec {
        ServiceSpec {
            entrypoint: entrypoint.to_string(),
            target: None,
            depends_on: depends_on
                .map(|deps| deps.into_iter().map(|value| value.to_string()).collect()),
            expose: None,
            env: None,
            state_bindings: Vec::new(),
            secrets: None,
            readiness_probe: None,
            network: None,
        }
    }

    fn resolved(name: &str, depends_on: &[&str], connections: &[&str]) -> ResolvedService {
        ResolvedService {
            name: name.to_string(),
            depends_on: depends_on.iter().map(|s| s.to_string()).collect(),
            connections: connections
                .iter()
                .map(|dep| ServiceConnectionInfo {
                    dependency: dep.to_string(),
                    host_env: format!("{}_HOST", dep.to_ascii_uppercase()),
                    port_env: format!("{}_PORT", dep.to_ascii_uppercase()),
                    container_port: None,
                    default_host: dep.to_string(),
                })
                .collect(),
            readiness_probe: None,
            network: ResolvedServiceNetwork::default(),
            run_once: false,
            runtime: ResolvedServiceRuntime::Oci(ResolvedTargetRuntime {
                target: name.to_string(),
                runtime: "oci".to_string(),
                driver: None,
                runtime_version: None,
                image: Some(format!("test/{}:latest", name)),
                entrypoint: String::new(),
                run_command: None,
                cmd: Vec::new(),
                env: Default::default(),
                working_dir: None,
                source_layout: None,
                port: None,
                required_env: Vec::new(),
                mounts: Vec::new(),
                user: None,
            }),
        }
    }

    #[test]
    fn graph_plan_respects_dependencies() {
        let mut services = HashMap::new();
        services.insert(
            "main".to_string(),
            service("node server.js", Some(vec!["api"])),
        );
        services.insert("api".to_string(), service("python api.py", None));

        let plan = ServiceGraphPlan::from_services(&services).unwrap();
        let main_idx = plan
            .startup_order()
            .iter()
            .position(|value| value == "main")
            .unwrap();
        let api_idx = plan
            .startup_order()
            .iter()
            .position(|value| value == "api")
            .unwrap();
        assert!(api_idx < main_idx);
        assert_eq!(
            plan.layers(),
            &[vec!["api".to_string()], vec!["main".to_string()]]
        );
    }

    #[test]
    fn graph_plan_rejects_cycles() {
        let mut services = HashMap::new();
        services.insert(
            "main".to_string(),
            service("node server.js", Some(vec!["api"])),
        );
        services.insert(
            "api".to_string(),
            service("python api.py", Some(vec!["main"])),
        );

        let err = ServiceGraphPlan::from_services(&services).unwrap_err();
        assert!(err.to_string().contains("circular dependency"));
    }

    #[test]
    fn graph_plan_rejects_unknown_dependencies() {
        let mut services = HashMap::new();
        services.insert(
            "main".to_string(),
            service("node server.js", Some(vec!["api"])),
        );

        let err = ServiceGraphPlan::from_services(&services).unwrap_err();
        assert!(err.to_string().contains("unknown service"));
    }

    /// AFFiNE-shape: db and redis are leaf sibling deps of migration, which is
    /// the sole dep of main. Verifies `from_orchestration` puts db + redis in
    /// the same leaf layer so the coordinator starts both before scheduling
    /// migration (the failure mode AODD PR #262 surfaced).
    #[test]
    fn from_orchestration_groups_affine_shape_sibling_leaves() {
        let plan = OrchestrationPlan {
            startup_order: vec![
                "db".to_string(),
                "redis".to_string(),
                "migration".to_string(),
                "main".to_string(),
            ],
            services: vec![
                resolved("db", &[], &[]),
                resolved("redis", &[], &[]),
                resolved("migration", &["db", "redis"], &["db", "redis"]),
                resolved("main", &["migration"], &["migration"]),
            ],
        };

        let graph = ServiceGraphPlan::from_orchestration(&plan).expect("layered plan");
        let layers = graph.layers();
        assert_eq!(layers.len(), 3, "expected 3 layers, got {:?}", layers);
        assert_eq!(layers[0], vec!["db".to_string(), "redis".to_string()]);
        assert_eq!(layers[1], vec!["migration".to_string()]);
        assert_eq!(layers[2], vec!["main".to_string()]);
    }

    /// Dify-shape: api depends on db + redis + weaviate (3 sibling leaves).
    /// All three should land in the same leaf layer.
    #[test]
    fn from_orchestration_groups_dify_shape_three_sibling_leaves() {
        let plan = OrchestrationPlan {
            startup_order: vec![
                "db".to_string(),
                "redis".to_string(),
                "weaviate".to_string(),
                "api".to_string(),
                "worker".to_string(),
                "main".to_string(),
            ],
            services: vec![
                resolved("db", &[], &[]),
                resolved("redis", &[], &[]),
                resolved("weaviate", &[], &[]),
                resolved(
                    "api",
                    &["db", "redis", "weaviate"],
                    &["db", "redis", "weaviate"],
                ),
                resolved(
                    "worker",
                    &["db", "redis", "weaviate"],
                    &["db", "redis", "weaviate"],
                ),
                resolved("main", &["api"], &["api"]),
            ],
        };

        let graph = ServiceGraphPlan::from_orchestration(&plan).expect("layered plan");
        let layers = graph.layers();
        assert_eq!(
            layers[0],
            vec![
                "db".to_string(),
                "redis".to_string(),
                "weaviate".to_string()
            ]
        );
        assert_eq!(layers[1], vec!["api".to_string(), "worker".to_string()]);
        assert_eq!(layers[2], vec!["main".to_string()]);
    }

    /// Defense-in-depth: even when a recipe declares cross-service edges only
    /// through `service.connections[].dependency` (not via `depends_on`), the
    /// graph builder must still infer the layer boundary. Mirrors the spec's
    /// `layers_include_connection_edges` requirement.
    #[test]
    fn from_orchestration_includes_connection_edges() {
        let plan = OrchestrationPlan {
            startup_order: vec!["db".to_string(), "main".to_string()],
            services: vec![
                resolved("db", &[], &[]),
                // No depends_on, but a connection to db.
                resolved("main", &[], &["db"]),
            ],
        };

        let graph = ServiceGraphPlan::from_orchestration(&plan).expect("layered plan");
        let layers = graph.layers();
        assert_eq!(layers.len(), 2, "expected 2 layers, got {:?}", layers);
        assert_eq!(layers[0], vec!["db".to_string()]);
        assert_eq!(layers[1], vec!["main".to_string()]);
    }

    /// Regression: Blinko-shape (single-service-with-db) keeps its existing
    /// layered behavior — db in layer 0, main in layer 1.
    #[test]
    fn from_orchestration_preserves_blinko_single_leaf_shape() {
        let plan = OrchestrationPlan {
            startup_order: vec!["db".to_string(), "main".to_string()],
            services: vec![resolved("db", &[], &[]), resolved("main", &["db"], &["db"])],
        };

        let graph = ServiceGraphPlan::from_orchestration(&plan).expect("layered plan");
        assert_eq!(
            graph.layers(),
            &[vec!["db".to_string()], vec!["main".to_string()]]
        );
    }
}
