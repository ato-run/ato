use std::collections::{HashMap, HashSet};

use crate::error::{CapsuleError, Result};
use crate::types::ServiceSpec;

pub fn startup_order_from_services(services: &HashMap<String, ServiceSpec>) -> Result<Vec<String>> {
    let dependencies = services
        .iter()
        .map(|(name, service)| (name.clone(), service.depends_on.clone().unwrap_or_default()))
        .collect();
    startup_order_from_dependencies(&dependencies)
}

/// Topological sort state for dependency ordering.
struct TopoSorter<'a> {
    dependencies: &'a HashMap<String, Vec<String>>,
    visited: &'a mut HashSet<String>,
    visiting: &'a mut HashSet<String>,
    out: &'a mut Vec<String>,
}

impl<'a> TopoSorter<'a> {
    fn visit(&mut self, current: &str, stack: &mut Vec<String>) -> Result<()> {
        if self.visited.contains(current) {
            return Ok(());
        }
        if self.visiting.contains(current) {
            stack.push(current.to_string());
            return Err(CapsuleError::Config(format!(
                "circular dependency detected: {}",
                stack.join(" -> ")
            )));
        }

        let deps = self
            .dependencies
            .get(current)
            .ok_or_else(|| CapsuleError::Config(format!("unknown service '{}'", current)))?;

        self.visiting.insert(current.to_string());
        stack.push(current.to_string());
        let deps = deps.clone();
        for dep in &deps {
            if !self.dependencies.contains_key(dep.as_str()) {
                return Err(CapsuleError::Config(format!(
                    "unknown service '{}' (depends_on)",
                    dep
                )));
            }
            self.visit(dep, stack)?;
        }
        stack.pop();
        self.visiting.remove(current);
        self.visited.insert(current.to_string());
        self.out.push(current.to_string());
        Ok(())
    }
}

pub fn startup_order_from_dependencies(
    dependencies: &HashMap<String, Vec<String>>,
) -> Result<Vec<String>> {
    let mut names: Vec<&String> = dependencies.keys().collect();
    names.sort();

    let mut out = Vec::new();
    let mut visited = HashSet::new();
    let mut visiting = HashSet::new();

    let mut sorter = TopoSorter {
        dependencies,
        visited: &mut visited,
        visiting: &mut visiting,
        out: &mut out,
    };

    for name in names {
        let mut stack = Vec::new();
        sorter.visit(name, &mut stack)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::startup_order_from_dependencies;
    use std::collections::HashMap;

    #[test]
    fn startup_order_sorts_dependencies() {
        let dependencies = HashMap::from([
            ("main".to_string(), vec!["db".to_string()]),
            ("db".to_string(), Vec::new()),
        ]);

        let order = startup_order_from_dependencies(&dependencies).expect("startup order");
        assert_eq!(order, vec!["db".to_string(), "main".to_string()]);
    }

    #[test]
    fn startup_order_rejects_cycles() {
        let dependencies = HashMap::from([
            ("main".to_string(), vec!["db".to_string()]),
            ("db".to_string(), vec!["main".to_string()]),
        ]);

        let err = startup_order_from_dependencies(&dependencies).unwrap_err();
        assert!(err.to_string().contains("circular dependency detected"));
    }
}
