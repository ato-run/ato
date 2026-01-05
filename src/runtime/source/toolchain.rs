//! Toolchain detection and version management
//!
//! Detects installed language runtimes and validates version compatibility.

use std::path::PathBuf;
use std::process::Command;

use tracing::{debug, warn};

/// Information about an installed toolchain
#[derive(Debug, Clone)]
pub struct ToolchainInfo {
    /// Language name (python, node, etc.)
    pub language: String,
    /// Detected version string
    pub version: String,
    /// Path to the binary
    pub path: PathBuf,
}

/// Manages detection and validation of host toolchains
pub struct ToolchainManager {
    /// Cached toolchain lookups
    cache: std::sync::Mutex<std::collections::HashMap<String, Option<ToolchainInfo>>>,
}

impl ToolchainManager {
    pub fn new() -> Self {
        Self {
            cache: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Find a compatible toolchain for the given language and version constraint
    pub fn find_toolchain(
        &self,
        language: &str,
        version_constraint: Option<&str>,
    ) -> Option<ToolchainInfo> {
        // Check cache first
        {
            let cache = self.cache.lock().unwrap();
            if let Some(cached) = cache.get(language) {
                if let Some(ref info) = cached {
                    if self.version_matches(&info.version, version_constraint) {
                        return Some(info.clone());
                    }
                }
                // Cached as not found
                if cached.is_none() {
                    return None;
                }
            }
        }

        // Try to detect toolchain
        let info = self.detect_toolchain(language);

        // Cache the result
        {
            let mut cache = self.cache.lock().unwrap();
            cache.insert(language.to_string(), info.clone());
        }

        // Check version constraint
        if let Some(ref info) = info {
            if self.version_matches(&info.version, version_constraint) {
                return Some(info.clone());
            } else {
                warn!(
                    "Toolchain {} version {} does not match constraint {:?}",
                    language, info.version, version_constraint
                );
                return None;
            }
        }

        None
    }

    /// Detect a toolchain by language name
    fn detect_toolchain(&self, language: &str) -> Option<ToolchainInfo> {
        let (binaries, version_args) = match language.to_lowercase().as_str() {
            "python" => (vec!["python3", "python"], vec!["--version"]),
            "node" | "nodejs" => (vec!["node"], vec!["--version"]),
            "deno" => (vec!["deno"], vec!["--version"]),
            "ruby" => (vec!["ruby"], vec!["--version"]),
            "perl" => (vec!["perl"], vec!["--version"]),
            _ => {
                warn!("Unknown language: {}", language);
                return None;
            }
        };

        for binary in binaries {
            if let Ok(path) = which::which(binary) {
                // Try to get version
                if let Ok(output) = Command::new(&path).args(&version_args).output() {
                    if output.status.success() {
                        let version_output = String::from_utf8_lossy(&output.stdout);
                        if let Some(version) = self.parse_version(language, &version_output) {
                            debug!("Found {} {} at {:?}", language, version, path);
                            return Some(ToolchainInfo {
                                language: language.to_string(),
                                version,
                                path,
                            });
                        }
                    }
                }
            }
        }

        None
    }

    /// Parse version string from command output
    fn parse_version(&self, language: &str, output: &str) -> Option<String> {
        let output = output.trim();

        match language.to_lowercase().as_str() {
            "python" => {
                // "Python 3.11.4"
                output.strip_prefix("Python ").map(|s| s.trim().to_string())
            }
            "node" | "nodejs" => {
                // "v18.17.0"
                output.strip_prefix('v').map(|s| s.trim().to_string())
            }
            "deno" => {
                // "deno 1.36.0"
                output.strip_prefix("deno ").map(|s| s.trim().to_string())
            }
            "ruby" => {
                // "ruby 3.2.0 ..."
                output
                    .strip_prefix("ruby ")
                    .and_then(|s| s.split_whitespace().next())
                    .map(|s| s.to_string())
            }
            "perl" => {
                // "This is perl 5, version 36, ..."
                // Or "perl, v5.36.0 ..."
                if output.contains("version") {
                    // Extract version number
                    output
                        .split("version")
                        .nth(1)
                        .and_then(|s| s.split(',').next())
                        .map(|s| format!("5.{}", s.trim()))
                } else {
                    output
                        .split('v')
                        .nth(1)
                        .and_then(|s| s.split_whitespace().next())
                        .map(|s| s.to_string())
                }
            }
            _ => Some(output.to_string()),
        }
    }

    /// Check if a version string matches a constraint
    fn version_matches(&self, version: &str, constraint: Option<&str>) -> bool {
        let constraint = match constraint {
            Some(c) => c,
            None => return true, // No constraint = any version
        };

        // Simple version matching
        // Supports: "3.11", "^3.11", ">=3.11", "3.11.4"
        let constraint = constraint.trim();

        if constraint.starts_with('^') {
            // Caret constraint: compatible versions (same major)
            let required = constraint.trim_start_matches('^');
            self.version_compatible(version, required)
        } else if constraint.starts_with(">=") {
            // Minimum version
            let required = constraint.trim_start_matches(">=");
            self.version_gte(version, required)
        } else if constraint.starts_with('>') {
            let required = constraint.trim_start_matches('>');
            self.version_gt(version, required)
        } else if constraint.starts_with("<=") {
            let required = constraint.trim_start_matches("<=");
            self.version_lte(version, required)
        } else if constraint.starts_with('<') {
            let required = constraint.trim_start_matches('<');
            self.version_lt(version, required)
        } else {
            // Exact or prefix match
            version.starts_with(constraint) || version == constraint
        }
    }

    /// Parse version into components
    fn parse_version_parts(&self, version: &str) -> Vec<u32> {
        version
            .split('.')
            .filter_map(|s| s.parse::<u32>().ok())
            .collect()
    }

    /// Check if version is compatible (same major version)
    fn version_compatible(&self, version: &str, required: &str) -> bool {
        let v_parts = self.parse_version_parts(version);
        let r_parts = self.parse_version_parts(required);

        if v_parts.is_empty() || r_parts.is_empty() {
            return false;
        }

        // Major version must match
        if v_parts[0] != r_parts[0] {
            return false;
        }

        // Must be >= required
        self.version_gte(version, required)
    }

    /// Version greater than or equal
    fn version_gte(&self, version: &str, required: &str) -> bool {
        let v_parts = self.parse_version_parts(version);
        let r_parts = self.parse_version_parts(required);

        for (v, r) in v_parts.iter().zip(r_parts.iter()) {
            if v > r {
                return true;
            }
            if v < r {
                return false;
            }
        }

        v_parts.len() >= r_parts.len()
    }

    /// Version greater than
    fn version_gt(&self, version: &str, required: &str) -> bool {
        let v_parts = self.parse_version_parts(version);
        let r_parts = self.parse_version_parts(required);

        for (v, r) in v_parts.iter().zip(r_parts.iter()) {
            if v > r {
                return true;
            }
            if v < r {
                return false;
            }
        }

        v_parts.len() > r_parts.len()
    }

    /// Version less than or equal
    fn version_lte(&self, version: &str, required: &str) -> bool {
        !self.version_gt(version, required)
    }

    /// Version less than
    fn version_lt(&self, version: &str, required: &str) -> bool {
        !self.version_gte(version, required)
    }
}

impl Default for ToolchainManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_python_version() {
        let tm = ToolchainManager::new();
        assert_eq!(
            tm.parse_version("python", "Python 3.11.4"),
            Some("3.11.4".to_string())
        );
    }

    #[test]
    fn test_parse_node_version() {
        let tm = ToolchainManager::new();
        assert_eq!(
            tm.parse_version("node", "v18.17.0"),
            Some("18.17.0".to_string())
        );
    }

    #[test]
    fn test_version_gte() {
        let tm = ToolchainManager::new();
        assert!(tm.version_gte("3.11.4", "3.11"));
        assert!(tm.version_gte("3.11.4", "3.11.4"));
        assert!(tm.version_gte("3.12.0", "3.11.4"));
        assert!(!tm.version_gte("3.10.0", "3.11"));
    }

    #[test]
    fn test_version_compatible() {
        let tm = ToolchainManager::new();
        // ^3.11 means 3.x where x >= 11
        assert!(tm.version_compatible("3.11.4", "3.11"));
        assert!(tm.version_compatible("3.12.0", "3.11"));
        assert!(!tm.version_compatible("4.0.0", "3.11"));
        assert!(!tm.version_compatible("3.10.0", "3.11"));
    }

    #[test]
    fn test_version_matches() {
        let tm = ToolchainManager::new();

        // No constraint
        assert!(tm.version_matches("3.11.4", None));

        // Exact match
        assert!(tm.version_matches("3.11.4", Some("3.11.4")));
        assert!(tm.version_matches("3.11.4", Some("3.11")));

        // Caret
        assert!(tm.version_matches("3.11.4", Some("^3.11")));
        assert!(!tm.version_matches("3.10.0", Some("^3.11")));

        // Comparison
        assert!(tm.version_matches("3.11.4", Some(">=3.11")));
        assert!(!tm.version_matches("3.10.0", Some(">=3.11")));
    }
}
