use std::collections::{BTreeSet, HashMap, VecDeque};

use anyhow::Result;
use serde_json::json;

use capsule_core::execution_plan::error::{
    AtoErrorClassification, AtoExecutionError, ManifestSuggestion,
};
use capsule_core::router::ManifestData;
use capsule_core::types::ServiceSpec;

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
    use capsule_core::types::ServiceSpec;

    fn service(entrypoint: &str, depends_on: Option<Vec<&str>>) -> ServiceSpec {
        ServiceSpec {
            entrypoint: entrypoint.to_string(),
            target: None,
            depends_on: depends_on
                .map(|deps| deps.into_iter().map(|value| value.to_string()).collect()),
            expose: None,
            env: None,
            state_bindings: Vec::new(),
            readiness_probe: None,
            network: None,
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
}
