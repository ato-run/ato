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
