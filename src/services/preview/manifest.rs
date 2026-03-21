#[derive(Debug, Default)]
pub(super) struct PreviewTomlSummary {
    pub driver: Option<String>,
    pub pack_include: Vec<String>,
    pub port: Option<u16>,
    pub runtime: Option<String>,
    pub runtime_version: Option<String>,
}

pub fn required_env_from_preview_toml(manifest_text: &str) -> Vec<String> {
    let Ok(parsed) = toml::from_str::<toml::Value>(manifest_text) else {
        return Vec::new();
    };

    let root_required_env = parsed
        .get("required_env")
        .and_then(toml::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(toml::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !root_required_env.is_empty() {
        return root_required_env;
    }

    parsed
        .get("env")
        .and_then(|env| env.get("required"))
        .and_then(toml::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(toml::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn summarize_preview_toml(manifest_text: &str) -> PreviewTomlSummary {
    let Ok(parsed) = toml::from_str::<toml::Value>(manifest_text) else {
        return PreviewTomlSummary::default();
    };

    let runtime = parsed
        .get("runtime")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let driver = runtime
        .as_deref()
        .and_then(|value| value.split('/').nth(1))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let runtime_version = parsed
        .get("runtime_version")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let port = parsed
        .get("port")
        .and_then(toml::Value::as_integer)
        .and_then(|value| u16::try_from(value).ok());
    let pack_include = parsed
        .get("pack")
        .and_then(|pack| pack.get("include"))
        .and_then(toml::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(toml::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    PreviewTomlSummary {
        driver,
        pack_include,
        port,
        runtime,
        runtime_version,
    }
}
