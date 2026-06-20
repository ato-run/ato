use capsule::execution_identity::{
    DependencyIdentity, EnvironmentIdentity, EnvironmentMode, FilesystemIdentity,
    ReproducibilityCause, ReproducibilityClass, ReproducibilityIdentity, RuntimeIdentity,
    TrackingStatus,
};
use capsule::execution_plan::model::ExecutionPlan;

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
    if environment
        .unknown_keys
        .iter()
        .any(|key| matches!(key.as_str(), "clock" | "time" | "timezone"))
    {
        causes.push(ReproducibilityCause::TimeBound);
    }
    if matches!(
        dependencies.derivation_hash.status,
        TrackingStatus::Unknown | TrackingStatus::Untracked
    ) || matches!(
        dependencies.output_hash.status,
        TrackingStatus::Unknown | TrackingStatus::Untracked
    ) {
        causes.push(ReproducibilityCause::UnknownDependencyOutput);
    }
    if runtime.binary_hash.status != TrackingStatus::Known {
        causes.push(ReproducibilityCause::UnknownRuntimeIdentity);
    }
    if runtime
        .dynamic_linkage
        .value
        .as_deref()
        .is_some_and(|value| value.starts_with("host:"))
    {
        causes.push(ReproducibilityCause::HostBound);
    }
    if runtime.dynamic_linkage.status == TrackingStatus::Untracked {
        causes.push(ReproducibilityCause::UntrackedDynamicDependency);
    }
    if environment.mode != EnvironmentMode::Closed
        || environment.closure_hash.status != TrackingStatus::Known
    {
        causes.push(ReproducibilityCause::UntrackedEnvironment);
    }
    if filesystem.view_hash.status != TrackingStatus::Known {
        causes.push(ReproducibilityCause::UntrackedFilesystemView);
    }
    causes.sort();
    causes.dedup();

    // The class-from-causes precedence lives in capsule-core so the OCI provider
    // assessment (#501) recomputes the class identically when it merges its causes.
    let class = ReproducibilityClass::from_causes(&causes);

    ReproducibilityIdentity { class, causes }
}

#[cfg(test)]
mod tests {
    use capsule::execution_identity::{
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
    fn state_bound_takes_precedence_over_host_and_network_bounds() {
        let result = classify_observations(
            true,
            &known_dependencies(),
            &known_runtime(Tracked::known("host:libcuda.so.1".to_string())),
            &known_environment(),
            &known_filesystem(vec!["state".to_string()]),
        );

        assert_eq!(result.class, ReproducibilityClass::StateBound);
        assert_eq!(
            result.causes,
            vec![
                ReproducibilityCause::HostBound,
                ReproducibilityCause::StateBound,
                ReproducibilityCause::NetworkBound
            ]
        );
    }

    /// #494: `NetworkBound` is an *egress-allowed capability* verdict, not an
    /// observation of traffic. The classifier's network input is a single
    /// boolean derived from policy (`!allow_hosts.is_empty()` in
    /// `classify_execution`); there is no observed-traffic input. Flipping the
    /// capability bool — with every other facet held known — is the sole
    /// difference between `Pure` and `NetworkBound`.
    #[test]
    fn network_bound_means_egress_allowed_not_observed_traffic() {
        let egress_allowed = classify_observations(
            true,
            &known_dependencies(),
            &known_runtime(Tracked::known("glibc:stable".to_string())),
            &known_environment(),
            &known_filesystem(Vec::new()),
        );
        assert_eq!(egress_allowed.class, ReproducibilityClass::NetworkBound);
        assert_eq!(
            egress_allowed.causes,
            vec![ReproducibilityCause::NetworkBound]
        );

        // Same execution, egress NOT permitted by policy ⇒ the cause is gone
        // and the class is Pure. The verdict tracks the policy capability, not
        // any observed network activity (of which the classifier sees none).
        let egress_denied = classify_observations(
            false,
            &known_dependencies(),
            &known_runtime(Tracked::known("glibc:stable".to_string())),
            &known_environment(),
            &known_filesystem(Vec::new()),
        );
        assert_eq!(egress_denied.class, ReproducibilityClass::Pure);
        assert!(
            !egress_denied
                .causes
                .contains(&ReproducibilityCause::NetworkBound)
        );
    }

    #[test]
    fn untracked_dynamic_linkage_is_best_effort_not_host_bound() {
        let result = classify_observations(
            false,
            &known_dependencies(),
            &known_runtime(Tracked::untracked(
                "dynamic linkage observer not implemented",
            )),
            &known_environment(),
            &known_filesystem(Vec::new()),
        );

        assert_eq!(result.class, ReproducibilityClass::BestEffort);
        assert_eq!(
            result.causes,
            vec![ReproducibilityCause::UntrackedDynamicDependency]
        );
    }

    #[test]
    fn dependency_not_applicable_does_not_prevent_pure() {
        let result = classify_observations(
            false,
            &DependencyIdentity {
                derivation_hash: Tracked::not_applicable(),
                output_hash: Tracked::not_applicable(),
            },
            &known_runtime(Tracked::known("glibc:stable".to_string())),
            &known_environment(),
            &known_filesystem(Vec::new()),
        );

        assert_eq!(result.class, ReproducibilityClass::Pure);
        assert!(result.causes.is_empty());
    }

    #[test]
    fn network_bound_without_unknowns_is_network_bound() {
        let result = classify_observations(
            true,
            &known_dependencies(),
            &known_runtime(Tracked::known("glibc:stable".to_string())),
            &known_environment(),
            &known_filesystem(Vec::new()),
        );

        assert_eq!(result.class, ReproducibilityClass::NetworkBound);
        assert_eq!(result.causes, vec![ReproducibilityCause::NetworkBound]);
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

    #[test]
    fn partial_environment_is_best_effort() {
        let result = classify_observations(
            false,
            &known_dependencies(),
            &known_runtime(Tracked::known("glibc:stable".to_string())),
            &EnvironmentIdentity {
                closure_hash: Tracked::known("blake3:env".to_string()),
                mode: EnvironmentMode::Partial,
                tracked_keys: vec!["PATH".to_string()],
                redacted_keys: Vec::new(),
                unknown_keys: vec!["timezone".to_string(), "umask".to_string()],
            },
            &known_filesystem(Vec::new()),
        );

        assert_eq!(result.class, ReproducibilityClass::BestEffort);
        assert_eq!(
            result.causes,
            vec![
                ReproducibilityCause::TimeBound,
                ReproducibilityCause::UntrackedEnvironment
            ]
        );
    }

    #[test]
    fn temporal_unknowns_are_time_bound_when_other_inputs_are_known() {
        let result = classify_observations(
            false,
            &known_dependencies(),
            &known_runtime(Tracked::known("glibc:stable".to_string())),
            &EnvironmentIdentity {
                closure_hash: Tracked::known("blake3:env".to_string()),
                mode: EnvironmentMode::Closed,
                tracked_keys: vec!["PATH".to_string()],
                redacted_keys: Vec::new(),
                unknown_keys: vec!["timezone".to_string()],
            },
            &known_filesystem(Vec::new()),
        );

        assert_eq!(result.class, ReproducibilityClass::TimeBound);
        assert_eq!(result.causes, vec![ReproducibilityCause::TimeBound]);
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
