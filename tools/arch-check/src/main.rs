use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use cargo_metadata::{DependencyKind, MetadataCommand, Package};

const FORBIDDEN_PACKAGES: &[&str] = &[
    "capsule-core",
    "capsule-core-codec",
    "capsule-compose",
    "capsule-protocol",
    "capsule-codec",
    "capsule-compat-v1",
    "capsule-adapter-state-io-v1",
    "capsule-session-runtime",
    "capsule",
    "capsulefs",
    "lock-draft-engine",
    "protocol",
    "ato-semantics-workspace",
    "ato-adapter-repository",
    "ato-provider-nacelle",
    "ato-provider-snapshot",
];

fn main() -> Result<()> {
    let metadata = MetadataCommand::new()
        .exec()
        .context("cargo metadata failed")?;
    let members: BTreeSet<_> = metadata.workspace_members.iter().collect();
    let packages: BTreeMap<_, _> = metadata
        .packages
        .iter()
        .filter(|package| members.contains(&package.id))
        .map(|package| (&package.id, package))
        .collect();
    let resolve = metadata
        .resolve
        .context("cargo metadata omitted resolve graph")?;
    let mut violations = Vec::new();

    for package in packages.values() {
        if FORBIDDEN_PACKAGES.contains(&package.name.as_str()) {
            violations.push(format!("forbidden legacy package exists: {}", package.name));
        }
        if package.name.starts_with("ato-provider-") {
            violations.push(format!(
                "provider architecture package exists: {}",
                package.name
            ));
        }
        let Some(source_layer) = layer(package) else {
            violations.push(format!("{} has no ato-architecture layer", package.name));
            continue;
        };
        let node = resolve
            .nodes
            .iter()
            .find(|node| node.id == package.id)
            .expect("workspace package has a resolve node");
        for dependency in &node.deps {
            if dependency
                .dep_kinds
                .iter()
                .all(|kind| kind.kind == DependencyKind::Development)
            {
                continue;
            }
            let Some(target) = packages.get(&dependency.pkg) else {
                continue;
            };
            let Some(target_layer) = layer(target) else {
                continue;
            };
            if !allowed(source_layer, target_layer) {
                violations.push(format!(
                    "{} ({source_layer}) must not depend on {} ({target_layer})",
                    package.name, target.name
                ));
            }
        }
    }

    if violations.is_empty() {
        println!("architecture valid: {} workspace packages", packages.len());
        return Ok(());
    }
    for violation in &violations {
        eprintln!("architecture violation: {violation}");
    }
    bail!("{} architecture violation(s)", violations.len())
}

fn layer(package: &Package) -> Option<&str> {
    package
        .metadata
        .get("ato-architecture")?
        .get("layer")?
        .as_str()
}

fn allowed(source: &str, target: &str) -> bool {
    match source {
        "computation" => false,
        "objects" => target == "computation",
        "kernel" => matches!(target, "computation" | "objects"),
        "compose" => matches!(target, "computation" | "kernel" | "objects"),
        "ipc" => target == "computation",
        "adapter-api" => matches!(target, "computation" | "objects"),
        "materializer-api" => matches!(target, "adapter-api" | "computation" | "objects"),
        "player" => matches!(target, "adapter-api" | "objects"),
        "adapter" => matches!(target, "adapter-api" | "computation" | "objects" | "ipc"),
        "materializer" => matches!(
            target,
            "adapter-api" | "materializer-api" | "computation" | "objects"
        ),
        "record-writer" => matches!(target, "adapter-api" | "computation" | "objects"),
        "contract-verifier" => matches!(target, "materializer-api" | "computation" | "objects"),
        "service" => matches!(target, "computation" | "objects" | "ipc"),
        "app" | "tool" => true,
        _ => false,
    }
}
