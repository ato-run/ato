use capsule_core::execution_identity::{
    DependencyIdentity, EnvironmentIdentity, EnvironmentMode, FilesystemIdentity,
    ReproducibilityCause, ReproducibilityClass, ReproducibilityIdentity, RuntimeIdentity,
    TrackingStatus,
};
use capsule_core::execution_plan::model::ExecutionPlan;

pub(crate) fn classify_execution(
    execution_plan: &ExecutionPlan,
    dependencies: &DependencyIdentity,
    runtime: &RuntimeIdentity,
    environment: &EnvironmentIdentity,
    filesystem: &FilesystemIdentity,
) -> ReproducibilityIdentity {
    classify_observations(
        !execution_plan.runtime.policy.network.allow_hosts.is_empty(),
        dependencies,
        runtime,
        environment,
        filesystem,
    )
}

fn classify_observations(
    network_bound: bool,
    dependencies: &DependencyIdentity,
    runtime: &RuntimeIdentity,
    environment: &EnvironmentIdentity,
    filesystem: &FilesystemIdentity,
) -> ReproducibilityIdentity {
    let mut causes = Vec::new();
    if network_bound {
        causes.push(ReproducibilityCause::NetworkBound);
    }
    if !filesystem.persistent_state.is_empty() {
        causes.push(ReproducibilityCause::StateBound);
    }
    if dependencies.output_hash.status != TrackingStatus::Known {
        causes.push(ReproducibilityCause::UnknownDependencyOutput);
    }
    if runtime.binary_hash.status != TrackingStatus::Known {
        causes.push(ReproducibilityCause::UnknownRuntimeIdentity);
    }
    if runtime.dynamic_linkage.status == TrackingStatus::Untracked {
        causes.push(ReproducibilityCause::HostBound);
    }
    if environment.mode == EnvironmentMode::Untracked
        || environment.closure_hash.status != TrackingStatus::Known
    {
        causes.push(ReproducibilityCause::UntrackedEnvironment);
    }
    if filesystem.view_hash.status != TrackingStatus::Known {
        causes.push(ReproducibilityCause::UntrackedFilesystemView);
    }
    causes.sort();
    causes.dedup();

    let class = if causes.is_empty() {
        ReproducibilityClass::Pure
    } else if causes.iter().any(is_best_effort_cause) {
        ReproducibilityClass::BestEffort
    } else {
        ReproducibilityClass::Bounded
    };

    ReproducibilityIdentity { class, causes }
}

fn is_best_effort_cause(cause: &ReproducibilityCause) -> bool {
    matches!(
        cause,
        ReproducibilityCause::UnknownDependencyOutput
            | ReproducibilityCause::UnknownRuntimeIdentity
            | ReproducibilityCause::UntrackedEnvironment
            | ReproducibilityCause::UntrackedFilesystemView
            | ReproducibilityCause::LifecycleUnknown
    )
}

#[cfg(test)]
mod tests {
    use capsule_core::execution_identity::{
        DependencyIdentity, EnvironmentIdentity, EnvironmentMode, FilesystemIdentity,
        PlatformIdentity, RuntimeIdentity, Tracked,
    };

    use super::*;

    #[test]
    fn pure_requires_all_critical_fields_known_and_no_bounds() {
        let result = classify_observations(
            false,
            &known_dependencies(),
            &known_runtime(Tracked::known("glibc:stable".to_string())),
            &known_environment(),
            &known_filesystem(Vec::new()),
        );

        assert_eq!(result.class, ReproducibilityClass::Pure);
        assert!(result.causes.is_empty());
    }

    #[test]
    fn unknown_dependency_output_is_best_effort() {
        let result = classify_observations(
            false,
            &DependencyIdentity {
                derivation_hash: Tracked::known("blake3:derivation".to_string()),
                output_hash: Tracked::unknown("not observed"),
            },
            &known_runtime(Tracked::known("glibc:stable".to_string())),
            &known_environment(),
            &known_filesystem(Vec::new()),
        );

        assert_eq!(result.class, ReproducibilityClass::BestEffort);
        assert_eq!(
            result.causes,
            vec![ReproducibilityCause::UnknownDependencyOutput]
        );
    }

    #[test]
    fn host_network_and_state_bounds_without_unknowns_are_bounded() {
        let result = classify_observations(
            true,
            &known_dependencies(),
            &known_runtime(Tracked::untracked(
                "dynamic linkage observer not implemented",
            )),
            &known_environment(),
            &known_filesystem(vec!["state".to_string()]),
        );

        assert_eq!(result.class, ReproducibilityClass::Bounded);
        assert_eq!(
            result.causes,
            vec![
                ReproducibilityCause::HostBound,
                ReproducibilityCause::StateBound,
                ReproducibilityCause::NetworkBound
            ]
        );
    }

    #[test]
    fn untracked_environment_and_filesystem_are_best_effort() {
        let result = classify_observations(
            false,
            &known_dependencies(),
            &known_runtime(Tracked::known("glibc:stable".to_string())),
            &EnvironmentIdentity {
                closure_hash: Tracked::untracked("not closed"),
                mode: EnvironmentMode::Untracked,
                tracked_keys: Vec::new(),
                redacted_keys: Vec::new(),
                unknown_keys: vec!["PATH".to_string()],
            },
            &FilesystemIdentity {
                view_hash: Tracked::untracked("not observed"),
                projection_strategy: "direct".to_string(),
                writable_dirs: Vec::new(),
                persistent_state: Vec::new(),
                known_readonly_layers: Vec::new(),
            },
        );

        assert_eq!(result.class, ReproducibilityClass::BestEffort);
        assert_eq!(
            result.causes,
            vec![
                ReproducibilityCause::UntrackedEnvironment,
                ReproducibilityCause::UntrackedFilesystemView
            ]
        );
    }

    fn known_dependencies() -> DependencyIdentity {
        DependencyIdentity {
            derivation_hash: Tracked::known("blake3:derivation".to_string()),
            output_hash: Tracked::known("blake3:output".to_string()),
        }
    }

    fn known_runtime(dynamic_linkage: Tracked<String>) -> RuntimeIdentity {
        RuntimeIdentity {
            declared: Some("node@20".to_string()),
            resolved: Some("/usr/bin/node".to_string()),
            binary_hash: Tracked::known("blake3:runtime".to_string()),
            dynamic_linkage,
            platform: PlatformIdentity {
                os: "macos".to_string(),
                arch: "arm64".to_string(),
                libc: "darwin".to_string(),
            },
        }
    }

    fn known_environment() -> EnvironmentIdentity {
        EnvironmentIdentity {
            closure_hash: Tracked::known("blake3:env".to_string()),
            mode: EnvironmentMode::Closed,
            tracked_keys: vec!["PATH".to_string()],
            redacted_keys: Vec::new(),
            unknown_keys: Vec::new(),
        }
    }

    fn known_filesystem(persistent_state: Vec<String>) -> FilesystemIdentity {
        FilesystemIdentity {
            view_hash: Tracked::known("blake3:fs".to_string()),
            projection_strategy: "direct".to_string(),
            writable_dirs: Vec::new(),
            persistent_state,
            known_readonly_layers: Vec::new(),
        }
    }
}
