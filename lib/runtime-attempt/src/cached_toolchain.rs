//! Runtime-local realization of an already provisioned, exact Python.
//! Never changes D/K or grants network access. The offline check runs in the
//! ordinary read-only, network-denied build sandbox, before any authored code.
use ato_formation::execution::{BuildAction, ExecutionPlan};
use ato_formation::intent::{
    DependencyPlan, Lane, Provisioning, ToolchainAccess, provision_steps, python_home,
};
use std::{collections::BTreeMap, path::Path};

pub(crate) fn resolve_python(plan: &mut ExecutionPlan, triple: &str) {
    resolve_python_with(plan, triple, |path| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(path)
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            false
        }
    });
}

fn resolve_python_with(plan: &mut ExecutionPlan, triple: &str, installed: impl Fn(&Path) -> bool) {
    let Some(version) = plan.toolchains.get("python") else {
        return;
    };
    let parts: Vec<_> = version.split('.').map(str::parse::<u32>).collect();
    let [Ok(major), Ok(minor), Ok(patch)] = parts.as_slice() else {
        return;
    };
    let executable = format!("{}/bin/python3", python_home(version));
    if !installed(Path::new(&executable)) {
        return;
    }
    // Compare the ENTIRE compiler-owned prerequisite, including its unforgeable
    // physical privilege. Matching a step's name alone would authorize source.
    let runtime = BTreeMap::from([("python".to_owned(), version.clone())]);
    let Ok((expected, _)) = provision_steps(
        &Provisioning {
            lane: Lane::Process,
            runtime: &runtime,
            dependencies: &DependencyPlan::None,
            static_build: None,
            static_compile: None,
            package_manager: None,
        },
        &plan.workspace_guest_root,
        triple,
    ) else {
        return;
    };
    let [expected] = expected.as_slice() else {
        return;
    };
    for action in &mut plan.actions {
        let BuildAction::Prerequisite(step) = action else {
            continue;
        };
        if step != expected {
            continue;
        }
        step.name = "verify-provisioned-python".into();
        step.argv = vec![
            executable.clone(),
            "-I".into(),
            "-S".into(),
            "-c".into(),
            format!(
                "import sys; assert sys.version_info[:3] == ({major}, {minor}, {patch}), 'pinned Python version mismatch'"
            ),
        ];
        step.needs_network = false;
        step.toolchain_access = ToolchainAccess::ReadOnly;
        // If the executable disappears/changes after planning, execution fails.
        // There is deliberately no download fallback under denied policy.
    }
}

/// Node's npm is part of the exact Node distribution, never a host executable.
/// Only the entire compiler-owned prerequisite is replaced by an offline check.
pub(crate) fn resolve_node(plan: &mut ExecutionPlan, triple: &str) {
    let Some(version) = plan.toolchains.get("node") else {
        return;
    };
    let executable = format!("{}/bin/node", ato_formation::intent::node_home(version));
    #[cfg(unix)]
    let installed = {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(&executable)
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    #[cfg(not(unix))]
    let installed = false;
    if !installed {
        return;
    }
    let runtime = BTreeMap::from([("node".to_owned(), version.clone())]);
    let Ok((steps, _)) = provision_steps(
        &Provisioning {
            lane: Lane::Process,
            runtime: &runtime,
            dependencies: &DependencyPlan::None,
            static_build: None,
            static_compile: None,
            package_manager: None,
        },
        &plan.workspace_guest_root,
        triple,
    ) else {
        return;
    };
    let [expected] = steps.as_slice() else {
        return;
    };
    for action in &mut plan.actions {
        if let BuildAction::Prerequisite(step) = action
            && step == expected
        {
            step.name = "verify-provisioned-node".into();
            step.argv = vec![
                executable.clone(),
                "-e".into(),
                format!(
                    "if (process.versions.node !== '{}') process.exit(1)",
                    version
                ),
            ];
            step.needs_network = false;
            step.toolchain_access = ToolchainAccess::ReadOnly;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const TRIPLE: &str = "aarch64-unknown-linux-gnu";
    fn plan() -> ExecutionPlan {
        let runtime = BTreeMap::from([("python".to_owned(), "3.12.7".to_owned())]);
        let (steps, path) = provision_steps(
            &Provisioning {
                lane: Lane::Process,
                runtime: &runtime,
                dependencies: &DependencyPlan::None,
                static_build: None,
                static_compile: None,
                package_manager: None,
            },
            "/app",
            TRIPLE,
        )
        .unwrap();
        ExecutionPlan {
            lane: Lane::Process,
            serving_step: 0,
            workspace_guest_root: "/app".into(),
            toolchains: runtime,
            package_manager: None,
            actions: steps.into_iter().map(BuildAction::Prerequisite).collect(),
            toolchain_path: path,
            environment_bindings: BTreeMap::new(),
        }
    }
    #[test]
    fn cached_python_uses_only_offline_read_only_exact_version_check() {
        let mut p = plan();
        let before = p.clone();
        resolve_python_with(&mut p, TRIPLE, |path| {
            assert_eq!(
                path,
                Path::new("/opt/ato/toolchains/python/3.12.7/bin/python3")
            );
            true
        });
        let BuildAction::Prerequisite(step) = &p.actions[0] else {
            panic!()
        };
        assert!(!step.needs_network);
        assert_eq!(step.toolchain_access, ToolchainAccess::ReadOnly);
        assert_eq!(&step.argv[1..4], &["-I", "-S", "-c"]);
        assert!(step.argv[4].contains("(3, 12, 7)"));
        assert!(
            !step
                .argv
                .iter()
                .any(|s| s.contains("curl") || s.contains("/bin/sh"))
        );
        p.actions = before.actions.clone();
        assert_eq!(p, before);
    }
    #[test]
    fn missing_python_keeps_network_prerequisite() {
        let mut p = plan();
        let before = p.clone();
        resolve_python_with(&mut p, TRIPLE, |_| false);
        assert_eq!(p, before);
    }
    #[test]
    fn authored_and_dependency_steps_are_never_reclassified() {
        let mut p = plan();
        let BuildAction::Prerequisite(mut dependency) = p.actions[0].clone() else {
            panic!()
        };
        dependency.argv.push("source-controlled".into());
        dependency.toolchain_access = ToolchainAccess::ReadOnly;
        p.actions.push(BuildAction::Prerequisite(dependency));
        p.actions.push(BuildAction::Authored { step: 1 });
        let other = p.actions[1..].to_vec();
        resolve_python_with(&mut p, TRIPLE, |_| true);
        assert_eq!(p.actions[1..], other);
        let BuildAction::Prerequisite(dependency) = &p.actions[1] else {
            panic!()
        };
        assert!(dependency.needs_network);
    }
}
