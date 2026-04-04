use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GuestContract {
    pub adapter: String,
    pub frontend_entry: PathBuf,
    pub transport: String,
    pub rpc_path: String,
    pub health_path: String,
    pub default_port: Option<u16>,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(super) struct GuestContractPreview {
    pub adapter: String,
    pub frontend_entry: String,
    pub transport: String,
    pub rpc_path: String,
    pub health_path: String,
    pub default_port: Option<u16>,
    pub capabilities: Vec<String>,
}

pub(super) fn parse_guest_contract(
    manifest: &toml::Value,
    manifest_dir: &Path,
) -> Option<GuestContract> {
    let table = manifest
        .get("metadata")
        .and_then(|value| value.get("desky_guest"))?
        .as_table()?;

    let adapter = table.get("adapter")?.as_str()?.trim().to_string();
    if adapter.is_empty() {
        return None;
    }

    let frontend_entry_raw = table.get("frontend_entry")?.as_str()?.trim();
    if frontend_entry_raw.is_empty() {
        return None;
    }

    let transport = table
        .get("transport")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("http")
        .to_string();
    let rpc_path = normalize_http_path(
        table
            .get("rpc_path")
            .and_then(|value| value.as_str())
            .unwrap_or("/rpc"),
    );
    let health_path = normalize_http_path(
        table
            .get("health_path")
            .and_then(|value| value.as_str())
            .unwrap_or("/health"),
    );
    let default_port = table
        .get("default_port")
        .and_then(|value| value.as_integer())
        .and_then(|value| u16::try_from(value).ok())
        .filter(|value| *value > 0);
    let capabilities = table
        .get("capabilities")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Some(GuestContract {
        adapter,
        frontend_entry: manifest_dir.join(frontend_entry_raw),
        transport,
        rpc_path,
        health_path,
        default_port,
        capabilities,
    })
}

pub(super) fn preview_guest_contract(contract: &GuestContract) -> GuestContractPreview {
    GuestContractPreview {
        adapter: contract.adapter.clone(),
        frontend_entry: contract.frontend_entry.display().to_string(),
        transport: contract.transport.clone(),
        rpc_path: contract.rpc_path.clone(),
        health_path: contract.health_path.clone(),
        default_port: contract.default_port,
        capabilities: contract.capabilities.clone(),
    }
}

fn normalize_http_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return "/".to_string();
    }
    if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_guest_contract_metadata() {
        let manifest: toml::Value = toml::from_str(
            r#"
[metadata.desky_guest]
adapter = "tauri"
frontend_entry = "frontend/index.html"
transport = "http"
rpc_path = "rpc"
health_path = "/health"
default_port = 43123
capabilities = ["app.invoke", "ping"]
"#,
        )
        .expect("parse toml");

        let contract = parse_guest_contract(&manifest, Path::new("/workspace"))
            .expect("guest contract");
        assert_eq!(contract.adapter, "tauri");
        assert_eq!(contract.frontend_entry, PathBuf::from("/workspace/frontend/index.html"));
        assert_eq!(contract.rpc_path, "/rpc");
        assert_eq!(contract.health_path, "/health");
        assert_eq!(contract.default_port, Some(43123));
        assert_eq!(contract.capabilities, vec!["app.invoke", "ping"]);
    }
}