use serde::{Deserialize, Serialize};

pub const CURRENT_SPEC_VERSION: &str = "1.0";
pub const LEGACY_SPEC_VERSION: &str = "0.1.0";

pub fn validate_spec_version(spec_version: &str) -> Result<(), String> {
    if is_supported_spec_version(spec_version) {
        return Ok(());
    }

    Err(format!(
        "Unsupported spec_version '{spec_version}'. Supported versions: {CURRENT_SPEC_VERSION}, {LEGACY_SPEC_VERSION}"
    ))
}

pub fn is_supported_spec_version(spec_version: &str) -> bool {
    spec_version == CURRENT_SPEC_VERSION || spec_version == LEGACY_SPEC_VERSION
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum NacelleEvent {
    IpcReady {
        service: String,
        endpoint: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        port: Option<u16>,
    },
    ServiceExited {
        service: String,
        exit_code: Option<i32>,
    },
}

impl NacelleEvent {
    pub fn emit(&self) {
        if let Ok(json) = serde_json::to_string(self) {
            println!("{}", json);
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supports_current_and_legacy_spec_versions() {
        assert!(is_supported_spec_version(CURRENT_SPEC_VERSION));
        assert!(is_supported_spec_version(LEGACY_SPEC_VERSION));
        assert!(validate_spec_version(CURRENT_SPEC_VERSION).is_ok());
        assert!(validate_spec_version(LEGACY_SPEC_VERSION).is_ok());
        assert!(validate_spec_version("2.0").is_err());
    }
}
