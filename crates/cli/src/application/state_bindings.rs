use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use capsule::AtoError;
use capsule::execution_plan::error::{AtoErrorClassification, AtoExecutionError};
use capsule::types::{CapsuleManifest, StateAttach, StateDurability};

use crate::state::ensure_registered_state_binding;

pub(crate) fn parse_attach_state_bindings(
    raw_bindings: &[String],
    cwd: &Path,
) -> Result<HashMap<String, String>> {
    let mut requested = HashMap::new();
    for raw in raw_bindings {
        let (state_name, path) = raw.split_once(':').ok_or_else(|| {
            anyhow::anyhow!(
                "invalid --attach-state binding '{}'; expected <state_name>:<path>",
                raw
            )
        })?;
        let state_name = state_name.trim();
        let path = path.trim();
        if state_name.is_empty() || path.is_empty() {
            anyhow::bail!(
                "invalid --attach-state binding '{}'; expected <state_name>:<path>",
                raw
            );
        }
        if requested
            .insert(state_name.to_string(), absolutize_state_path(path, cwd))
            .is_some()
        {
            anyhow::bail!(
                "state '{}' was bound more than once via --attach-state",
                state_name
            );
        }
    }
    Ok(requested)
}

pub(crate) fn resolve_attach_state_source_overrides(
    manifest: &CapsuleManifest,
    raw_bindings: &[String],
    cwd: &Path,
) -> Result<HashMap<String, String>> {
    let requested = parse_attach_state_bindings(raw_bindings, cwd)?;
    resolve_attach_state_source_overrides_from_requested(manifest, &requested)
}

pub(crate) fn require_explicit_persistent_state_bindings(manifest: &CapsuleManifest) -> Result<()> {
    let empty = HashMap::new();
    resolve_attach_state_source_overrides_from_requested(manifest, &empty).map(|_| ())
}

fn resolve_attach_state_source_overrides_from_requested(
    manifest: &CapsuleManifest,
    requested: &HashMap<String, String>,
) -> Result<HashMap<String, String>> {
    for state_name in requested.keys() {
        let requirement = manifest.state.get(state_name).ok_or_else(|| {
            anyhow::anyhow!(
                "--attach-state references undeclared manifest state '{}'",
                state_name
            )
        })?;
        if requirement.durability != StateDurability::Persistent {
            anyhow::bail!(
                "--attach-state only supports persistent manifest state; '{}' is {:?}",
                state_name,
                requirement.durability
            );
        }
    }

    let explicit_persistent_states: Vec<_> = manifest
        .state
        .iter()
        .filter(|(_, requirement)| {
            requirement.durability == StateDurability::Persistent
                && requirement.attach == StateAttach::Explicit
        })
        .collect();
    if explicit_persistent_states.is_empty() && requested.is_empty() {
        return Ok(HashMap::new());
    }

    let mut resolved = HashMap::new();
    for (state_name, locator) in requested {
        let record = ensure_registered_state_binding(manifest, state_name, locator)
            .with_context(|| format!("failed to bind persistent state '{}'", state_name))?;
        resolved.insert(state_name.clone(), record.backend_locator);
    }

    for (state_name, _) in explicit_persistent_states {
        if resolved.contains_key(state_name) {
            continue;
        }
        let locator = requested
            .get(state_name.as_str())
            .ok_or_else(|| missing_attach_state_error(state_name))?;
        let record = ensure_registered_state_binding(manifest, state_name, locator)
            .with_context(|| format!("failed to bind persistent state '{}'", state_name))?;
        resolved.insert(state_name.clone(), record.backend_locator);
    }

    Ok(resolved)
}

fn missing_attach_state_error(state_name: &str) -> anyhow::Error {
    AtoExecutionError::from_ato_error(AtoError::ExecutionContractInvalid {
        message: format!(
            "state '{state_name}' requires an explicit persistent binding.\nPass: --attach-state {state_name}:/path/to/{state_name}"
        ),
        hint: Some(format!(
            "Pass: --attach-state {state_name}:/path/to/{state_name}"
        )),
        field: Some(state_name.to_string()),
        service: None,
    })
    .with_classification(AtoErrorClassification::Manifest)
    .into()
}

fn absolutize_state_path(path: &str, cwd: &Path) -> String {
    let path = PathBuf::from(path);
    let absolute = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    absolute
        .canonicalize()
        .unwrap_or(absolute)
        .to_string_lossy()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_attach_state_binding_accepts_single_and_multiple() {
        let cwd = Path::new("/workspace");
        let parsed =
            parse_attach_state_bindings(&["data:/tmp/memos-data".to_string()], cwd).unwrap();
        assert_eq!(
            parsed.get("data").map(String::as_str),
            Some("/tmp/memos-data")
        );

        let parsed = parse_attach_state_bindings(
            &["data:/tmp/data".to_string(), "db-data:/tmp/db".to_string()],
            cwd,
        )
        .unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed.get("db-data").map(String::as_str), Some("/tmp/db"));
    }

    #[test]
    fn parse_attach_state_binding_rejects_invalid_shapes() {
        let cwd = Path::new("/workspace");
        for value in ["data", ":path", "data:"] {
            let err = parse_attach_state_bindings(&[value.to_string()], cwd).unwrap_err();
            assert!(err.to_string().contains("--attach-state"));
        }
    }

    #[test]
    fn parse_attach_state_binding_rejects_duplicate_state_name() {
        let cwd = Path::new("/workspace");
        let err = parse_attach_state_bindings(
            &["data:/tmp/one".to_string(), "data:/tmp/two".to_string()],
            cwd,
        )
        .unwrap_err();
        assert!(err.to_string().contains("more than once"));
    }

    #[test]
    fn missing_binding_is_required_only_for_explicit_persistent_state() {
        let manifest = CapsuleManifest::from_toml(
            r#"
schema_version = "0.3"
name = "state-test"
type = "app"

[state.cache]
kind = "filesystem"
durability = "persistent"
purpose = "cache"
attach = "auto"
"#,
        )
        .expect("manifest");

        require_explicit_persistent_state_bindings(&manifest)
            .expect("non-explicit persistent state should keep existing semantics");

        let manifest = CapsuleManifest::from_toml(
            r#"
schema_version = "0.3"
name = "state-test"
type = "app"

[state.data]
kind = "filesystem"
durability = "persistent"
purpose = "data"
attach = "explicit"
"#,
        )
        .expect("manifest");

        let err = require_explicit_persistent_state_bindings(&manifest)
            .expect_err("explicit persistent state should require binding");
        assert!(
            err.to_string()
                .contains("--attach-state data:/path/to/data")
        );
    }
}
