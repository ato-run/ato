use std::collections::{BTreeMap, HashMap};

pub fn normalized_python_runtime_version(runtime_version: Option<&str>) -> Option<String> {
    runtime_version
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

pub fn python_selector_env(runtime_version: Option<&str>) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    let Some(runtime_version) = normalized_python_runtime_version(runtime_version) else {
        return env;
    };

    env.insert("UV_MANAGED_PYTHON".to_string(), "1".to_string());
    env.insert("UV_PYTHON".to_string(), runtime_version);
    env
}

pub fn extend_python_selector_env(
    env: &mut HashMap<String, String>,
    runtime_version: Option<&str>,
) {
    env.extend(python_selector_env(runtime_version));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_selector_env_is_empty_without_runtime_version() {
        assert!(python_selector_env(None).is_empty());
        assert!(python_selector_env(Some("   ")).is_empty());
    }

    #[test]
    fn python_selector_env_derives_uv_transport_from_runtime_version() {
        let env = python_selector_env(Some("3.11.10"));
        assert_eq!(env.get("UV_MANAGED_PYTHON").map(String::as_str), Some("1"));
        assert_eq!(env.get("UV_PYTHON").map(String::as_str), Some("3.11.10"));
    }
}
