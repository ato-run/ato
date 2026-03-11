use std::collections::{HashMap, HashSet};

use anyhow::Result;

use crate::types::ServiceSpec;

pub fn startup_order_from_services(services: &HashMap<String, ServiceSpec>) -> Result<Vec<String>> {
    let dependencies = services
        .iter()
        .map(|(name, service)| (name.clone(), service.depends_on.clone().unwrap_or_default()))
        .collect();
    startup_order_from_dependencies(&dependencies)
}

pub fn startup_order_from_dependencies(
    dependencies: &HashMap<String, Vec<String>>,
) -> Result<Vec<String>> {
    fn visit(
        current: &str,
        dependencies: &HashMap<String, Vec<String>>,
        visited: &mut HashSet<String>,
        visiting: &mut HashSet<String>,
        stack: &mut Vec<String>,
        out: &mut Vec<String>,
    ) -> Result<()> {
        if visited.contains(current) {
            return Ok(());
        }
        if visiting.contains(current) {
            stack.push(current.to_string());
            anyhow::bail!("circular dependency detected: {}", stack.join(" -> "));
        }

        let deps = dependencies
            .get(current)
            .ok_or_else(|| anyhow::anyhow!("unknown service '{}'", current))?;

        visiting.insert(current.to_string());
        stack.push(current.to_string());
        for dep in deps {
            if !dependencies.contains_key(dep) {
                anyhow::bail!("unknown service '{}' (depends_on)", dep);
            }
            visit(dep, dependencies, visited, visiting, stack, out)?;
        }
        stack.pop();
        visiting.remove(current);
        visited.insert(current.to_string());
        out.push(current.to_string());
        Ok(())
    }

    let mut names: Vec<&String> = dependencies.keys().collect();
    names.sort();

    let mut out = Vec::new();
    let mut visited = HashSet::new();
    let mut visiting = HashSet::new();
    for name in names {
        let mut stack = Vec::new();
        visit(
            name,
            dependencies,
            &mut visited,
            &mut visiting,
            &mut stack,
            &mut out,
        )?;
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
