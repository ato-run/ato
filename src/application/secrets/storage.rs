/// Detect whether the OS keychain is usable in this environment.
///
/// Returns `false` in CI/headless environments where the keychain
/// daemon is unavailable, or when a probe write/read fails.
/// Always returns `false` in unit-test builds to avoid OS permission dialogs.
pub(crate) fn is_keyring_available() -> bool {
    #[cfg(test)]
    {
        return false;
    }
    #[cfg(not(test))]
    {
        // In known CI environments, skip the probe to avoid spurious errors.
        if is_ci_environment() {
            return false;
        }

        // Attempt a non-destructive probe: set and immediately delete a test entry.
        use keyring::Entry;
        let Ok(entry) = Entry::new("ato.secrets.probe", "__probe__") else {
            return false;
        };
        if entry.set_password("probe").is_err() {
            return false;
        }
        let _ = entry.delete_password();
        true
    }
}

/// Returns `true` when running inside a known CI/CD environment.
pub(crate) fn is_ci_environment() -> bool {
    const CI_VARS: &[&str] = &[
        "CI",
        "CONTINUOUS_INTEGRATION",
        "GITHUB_ACTIONS",
        "GITLAB_CI",
        "CIRCLECI",
        "BUILDKITE",
        "TF_BUILD",
        "TRAVIS",
        "JENKINS_URL",
        "TEAMCITY_VERSION",
        "BITBUCKET_BUILD_NUMBER",
    ];
    CI_VARS
        .iter()
        .any(|var| std::env::var(var).map(|v| !v.is_empty()).unwrap_or(false))
}
