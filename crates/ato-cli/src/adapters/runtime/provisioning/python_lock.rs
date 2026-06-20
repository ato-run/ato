use capsule::AtoError;
use capsule::execution_plan::error::AtoExecutionError;

pub(crate) fn python_requirements_lock_hint() -> &'static str {
    "Run with `--auto-fix:all` to generate an Ato-compatible pip-compile uv.lock in the GitHub checkout, or run `uv pip compile requirements.txt -o uv.lock` and commit the generated lockfile upstream."
}

pub(crate) fn python_requirements_lock_missing(message: impl Into<String>) -> AtoExecutionError {
    AtoExecutionError::from_ato_error(AtoError::DependencyLockMissing {
        message: message.into(),
        hint: Some(python_requirements_lock_hint().to_string()),
        lockfile: "uv.lock".to_string(),
        package_manager: Some("uv".to_string()),
        target: Some("uv.lock".to_string()),
    })
}

pub(crate) fn python_requirements_lock_sync_command(runtime_version: Option<&str>) -> String {
    let python_pin = runtime_version
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!(" --python {value}"))
        .unwrap_or_default();

    format!("uv venv{python_pin} --seed --clear && uv pip sync uv.lock")
}
