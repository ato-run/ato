//! IPC validation rules for `ato validate` and `ato pack`.
//!
//! Checks IPC-specific constraints in `capsule.toml`:
//! - `[ipc.exports.sharing]` and `[lifecycle]` shortcut mutual exclusion
//! - Reserved prefix (`_ipc_`, `_setup`, `_main`) collision detection
//! - Circular dependency detection in IPC import graph
//! - `remote = true` capability warnings
//! - `from` resolvability checks (local store existence)

use std::path::Path;

use anyhow::{Context, Result};

use super::dag::{self, DagError, RESERVED_PREFIXES};
use super::schema;
use super::types::IpcConfig;

/// A single IPC validation diagnostic.
#[derive(Debug, Clone)]
pub struct IpcDiagnostic {
    /// Severity: error or warning.
    pub severity: Severity,
    /// Diagnostic code (e.g., "IPC-001").
    pub code: &'static str,
    /// Human-readable message.
    pub message: String,
    /// Where in capsule.toml this was found.
    pub location: String,
    /// Suggested fix.
    pub hint: String,
}

/// Severity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl std::fmt::Display for IpcDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let icon = match self.severity {
            Severity::Error => "❌",
            Severity::Warning => "⚠️",
        };
        write!(
            f,
            "{} [{}] {} (at {})\n   Hint: {}",
            icon, self.code, self.message, self.location, self.hint
        )
    }
}

/// Validate all IPC-related rules in a capsule manifest.
///
/// Returns a list of diagnostics (errors and warnings).
/// If the list contains any `Severity::Error`, the manifest should be rejected.
pub fn validate_ipc(
    ipc_config: &IpcConfig,
    raw_toml: &toml::Value,
    capsule_root: &Path,
    service_names: &[String],
) -> Vec<IpcDiagnostic> {
    let mut diagnostics = Vec::new();

    // Rule 1: [ipc.exports.sharing] + [lifecycle] mutual exclusion
    check_sharing_lifecycle_conflict(ipc_config, raw_toml, &mut diagnostics);

    // Rule 2: Reserved prefix collision
    check_reserved_prefixes(ipc_config, service_names, &mut diagnostics);

    // Rule 3: Circular dependency detection
    check_circular_dependencies(ipc_config, service_names, &mut diagnostics);

    // Rule 4: remote=true capability warnings
    check_remote_capabilities(ipc_config, &mut diagnostics);

    // Rule 5: from resolvability
    check_import_resolvability(ipc_config, capsule_root, &mut diagnostics);

    // Rule 6: Empty exports validation
    check_empty_exports(ipc_config, &mut diagnostics);

    // Rule 7: Referenced JSON Schema files
    check_schema_files(ipc_config, capsule_root, &mut diagnostics);

    diagnostics
}

/// Parse the manifest-level `[ipc]` section and run all IPC validators.
pub fn validate_manifest(
    raw_toml: &toml::Value,
    capsule_root: &Path,
) -> Result<Vec<IpcDiagnostic>> {
    let Some(ipc_table) = raw_toml.get("ipc") else {
        return Ok(Vec::new());
    };

    let ipc_str = toml::to_string(ipc_table).context("Failed to serialize [ipc] section")?;
    let config: IpcConfig =
        toml::from_str(&ipc_str).context("Failed to parse [ipc] section for validation")?;

    let service_names = raw_toml
        .get("services")
        .and_then(|value| value.as_table())
        .map(|table| table.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();

    Ok(validate_ipc(
        &config,
        raw_toml,
        capsule_root,
        &service_names,
    ))
}

/// IPC-001: `[ipc.exports.sharing]` and `[lifecycle]` shortcut cannot coexist.
fn check_sharing_lifecycle_conflict(
    ipc_config: &IpcConfig,
    raw_toml: &toml::Value,
    diagnostics: &mut Vec<IpcDiagnostic>,
) {
    let has_ipc_sharing = ipc_config.exports.as_ref().is_some_and(|e| {
        // Check if sharing was explicitly set (not default)
        e.sharing.mode != super::types::SharingMode::Singleton
            || e.sharing.idle_timeout != 300
            || e.sharing.max_clients != 0
    });

    let has_lifecycle = raw_toml.get("lifecycle").is_some();

    if has_ipc_sharing && has_lifecycle {
        diagnostics.push(IpcDiagnostic {
            severity: Severity::Error,
            code: "IPC-001",
            message: "[ipc.exports.sharing] and [lifecycle] cannot be used together".to_string(),
            location: "[ipc.exports.sharing] + [lifecycle]".to_string(),
            hint: "Use either [ipc.exports.sharing] for IPC lifecycle or [lifecycle] \
                   for legacy lifecycle, not both. IPC sharing mode supersedes [lifecycle]."
                .to_string(),
        });
    }
}

/// IPC-002: Service names must not use reserved prefixes.
fn check_reserved_prefixes(
    ipc_config: &IpcConfig,
    service_names: &[String],
    diagnostics: &mut Vec<IpcDiagnostic>,
) {
    // Check user-defined service names
    for name in service_names {
        for prefix in RESERVED_PREFIXES {
            if name.starts_with(prefix) {
                diagnostics.push(IpcDiagnostic {
                    severity: Severity::Error,
                    code: "IPC-002",
                    message: format!("Service name '{}' uses reserved prefix '{}'", name, prefix),
                    location: format!("[services.{}]", name),
                    hint: format!(
                        "Rename the service to avoid the '{}' prefix. \
                         This prefix is reserved for IPC system use.",
                        prefix
                    ),
                });
            }
        }
    }

    // Check IPC export name
    if let Some(exports) = &ipc_config.exports {
        if let Some(ref name) = exports.name {
            for prefix in RESERVED_PREFIXES {
                if name.starts_with(prefix) {
                    diagnostics.push(IpcDiagnostic {
                        severity: Severity::Error,
                        code: "IPC-002",
                        message: format!(
                            "IPC export name '{}' uses reserved prefix '{}'",
                            name, prefix
                        ),
                        location: "[ipc.exports]".to_string(),
                        hint: format!("Rename the export to avoid the '{}' prefix.", prefix),
                    });
                }
            }
        }
    }

    // Check IPC import names
    for import_name in ipc_config.imports.keys() {
        for prefix in RESERVED_PREFIXES {
            if import_name.starts_with(prefix) {
                diagnostics.push(IpcDiagnostic {
                    severity: Severity::Error,
                    code: "IPC-002",
                    message: format!(
                        "IPC import name '{}' uses reserved prefix '{}'",
                        import_name, prefix
                    ),
                    location: format!("[ipc.imports.{}]", import_name),
                    hint: format!("Rename the import to avoid the '{}' prefix.", prefix),
                });
            }
        }
    }
}

/// IPC-003: Circular dependency detection.
fn check_circular_dependencies(
    ipc_config: &IpcConfig,
    service_names: &[String],
    diagnostics: &mut Vec<IpcDiagnostic>,
) {
    if ipc_config.imports.is_empty() {
        return;
    }

    match dag::build_ipc_dag(&ipc_config.imports, service_names, "_main") {
        Ok(_) => {} // No cycle
        Err(DagError::CyclicDependency { cycle }) => {
            diagnostics.push(IpcDiagnostic {
                severity: Severity::Error,
                code: "IPC-003",
                message: format!("Circular IPC dependency detected: {}", cycle),
                location: "[ipc.imports]".to_string(),
                hint: "Break the circular dependency by making one of the imports \
                       lazy (activation = \"lazy\") or optional (optional = true)."
                    .to_string(),
            });
        }
        Err(DagError::ReservedPrefix { name, prefix }) => {
            // Already caught by IPC-002, but add if it slipped through
            diagnostics.push(IpcDiagnostic {
                severity: Severity::Error,
                code: "IPC-002",
                message: format!(
                    "Service '{}' uses reserved prefix '{}' (detected during DAG build)",
                    name, prefix
                ),
                location: "[ipc.imports]".to_string(),
                hint: format!("Rename '{}' to avoid reserved prefix.", name),
            });
        }
        Err(DagError::DuplicateNode { name }) => {
            diagnostics.push(IpcDiagnostic {
                severity: Severity::Error,
                code: "IPC-004",
                message: format!("Duplicate IPC node name: '{}'", name),
                location: "[ipc.imports]".to_string(),
                hint: "Ensure all IPC import names are unique.".to_string(),
            });
        }
    }
}

/// IPC-005: Warn about remote capabilities.
fn check_remote_capabilities(ipc_config: &IpcConfig, diagnostics: &mut Vec<IpcDiagnostic>) {
    if let Some(exports) = &ipc_config.exports {
        for method in &exports.methods {
            // Check if method description mentions "remote" capability
            // In a full implementation this would check a `remote: bool` field
            if method.description.to_lowercase().contains("remote") {
                diagnostics.push(IpcDiagnostic {
                    severity: Severity::Warning,
                    code: "IPC-005",
                    message: format!(
                        "Method '{}' appears to be a remote capability. \
                         Remote IPC calls bypass the local sandbox.",
                        method.name
                    ),
                    location: format!("[ipc.exports.methods.{}]", method.name),
                    hint: "Ensure this capability is intentionally exposed to remote callers. \
                           Consider adding rate limiting and authentication."
                        .to_string(),
                });
            }
        }
    }
}

/// IPC-006: Check that `from` values in imports can be resolved.
fn check_import_resolvability(
    ipc_config: &IpcConfig,
    capsule_root: &Path,
    diagnostics: &mut Vec<IpcDiagnostic>,
) {
    for (import_name, config) in &ipc_config.imports {
        let from = &config.from;

        if from.starts_with("./") || from.starts_with("../") {
            // Relative path — check if it exists
            let resolved = capsule_root.join(from);
            if !resolved.exists() {
                let severity = if config.optional {
                    Severity::Warning
                } else {
                    Severity::Error
                };
                diagnostics.push(IpcDiagnostic {
                    severity,
                    code: "IPC-006",
                    message: format!(
                        "Import '{}': relative path '{}' does not exist (resolved: {})",
                        import_name,
                        from,
                        resolved.display()
                    ),
                    location: format!("[ipc.imports.{}]", import_name),
                    hint: format!(
                        "Check the path '{}' relative to {}. \
                         If the dependency is from the registry, use a name or scoped identifier.",
                        from,
                        capsule_root.display()
                    ),
                });
            }
        } else {
            // Named dependency — check local store
            let store_path = if from.starts_with('@') {
                let without_at = from.strip_prefix('@').unwrap_or(from);
                let name = without_at.split(':').next().unwrap_or(without_at);
                capsule_core::common::paths::ato_path_or_workspace_tmp("store")
                    .join(format!("@{}", name))
            } else {
                capsule_core::common::paths::ato_path_or_workspace_tmp("store").join(from)
            };

            if !store_path.exists() {
                let severity = if config.optional {
                    Severity::Warning
                } else {
                    Severity::Error
                };
                diagnostics.push(IpcDiagnostic {
                    severity,
                    code: "IPC-006",
                    message: format!(
                        "Import '{}': service '{}' not found in local store",
                        import_name, from
                    ),
                    location: format!("[ipc.imports.{}]", import_name),
                    hint: format!("Install the dependency first: ato install {}", from),
                });
            }
        }
    }
}

/// IPC-007: Empty exports validation.
fn check_empty_exports(ipc_config: &IpcConfig, diagnostics: &mut Vec<IpcDiagnostic>) {
    if let Some(exports) = &ipc_config.exports {
        if exports.methods.is_empty() && exports.name.is_some() {
            diagnostics.push(IpcDiagnostic {
                severity: Severity::Warning,
                code: "IPC-007",
                message: format!(
                    "IPC exports section has a name ('{}') but no methods defined",
                    exports.name.as_deref().unwrap_or("unnamed")
                ),
                location: "[ipc.exports]".to_string(),
                hint: "Add methods to [ipc.exports.methods] or remove [ipc.exports] \
                       if this capsule does not provide IPC services."
                    .to_string(),
            });
        }
    }
}

/// IPC-008: Referenced JSON Schema files must exist and compile.
fn check_schema_files(
    ipc_config: &IpcConfig,
    capsule_root: &Path,
    diagnostics: &mut Vec<IpcDiagnostic>,
) {
    let Some(exports) = ipc_config.exports.as_ref() else {
        return;
    };

    for method in &exports.methods {
        if let Some(schema_path) = method.input_schema.as_deref() {
            check_single_schema_file(
                method,
                "input_schema",
                schema_path,
                capsule_root,
                diagnostics,
            );
        }
        if let Some(schema_path) = method.output_schema.as_deref() {
            check_single_schema_file(
                method,
                "output_schema",
                schema_path,
                capsule_root,
                diagnostics,
            );
        }
    }
}

fn check_single_schema_file(
    method: &super::types::IpcMethodDescriptor,
    field: &'static str,
    schema_path: &str,
    capsule_root: &Path,
    diagnostics: &mut Vec<IpcDiagnostic>,
) {
    match schema::load_schema_value(schema_path, capsule_root)
        .and_then(|value| schema::validate_schema_definition(&value, schema_path))
    {
        Ok(()) => {}
        Err(err) => diagnostics.push(IpcDiagnostic {
            severity: Severity::Error,
            code: "IPC-008",
            message: format!(
                "Method '{}' references invalid {} '{}': {}",
                method.name, field, schema_path, err
            ),
            location: format!("[ipc.exports.methods.{}.{}]", method.name, field),
            hint: "Provide a readable JSON Schema file relative to the capsule root.".to_string(),
        }),
    }
}

/// Check if any diagnostics contain errors (vs only warnings).
pub fn has_errors(diagnostics: &[IpcDiagnostic]) -> bool {
    diagnostics.iter().any(|d| d.severity == Severity::Error)
}

/// Format all diagnostics for human-readable output.
pub fn format_diagnostics(diagnostics: &[IpcDiagnostic]) -> String {
    if diagnostics.is_empty() {
        return "✅ IPC configuration valid.".to_string();
    }

    let errors = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count();
    let warnings = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Warning)
        .count();

    let mut output = String::new();
    for diag in diagnostics {
        output.push_str(&format!("{}\n", diag));
    }
    output.push_str(&format!(
        "\nIPC validation: {} error(s), {} warning(s)\n",
        errors, warnings
    ));
    output
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::types::{
        ActivationMode, IpcConfig, IpcExportsConfig, IpcImportConfig, IpcMethodDescriptor,
        IpcSharingConfig, SharingMode,
    };
    use std::collections::HashMap;

    fn empty_config() -> IpcConfig {
        IpcConfig {
            exports: None,
            imports: HashMap::new(),
        }
    }

    fn empty_toml() -> toml::Value {
        toml::Value::Table(toml::map::Map::new())
    }

    fn toml_with_lifecycle() -> toml::Value {
        let mut map = toml::map::Map::new();
        let mut lifecycle = toml::map::Map::new();
        lifecycle.insert("shutdown_timeout".to_string(), toml::Value::Integer(30));
        map.insert("lifecycle".to_string(), toml::Value::Table(lifecycle));
        toml::Value::Table(map)
    }

    #[test]
    fn test_empty_config_is_valid() {
        let config = empty_config();
        let diagnostics = validate_ipc(&config, &empty_toml(), Path::new("/tmp"), &[]);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn test_ipc_001_sharing_lifecycle_conflict() {
        let config = IpcConfig {
            exports: Some(IpcExportsConfig {
                name: Some("my-service".to_string()),
                methods: vec![],
                sharing: IpcSharingConfig {
                    mode: SharingMode::Daemon,
                    idle_timeout: 300,
                    max_clients: 0,
                },
            }),
            imports: HashMap::new(),
        };

        let diagnostics = validate_ipc(&config, &toml_with_lifecycle(), Path::new("/tmp"), &[]);
        assert!(diagnostics.iter().any(|d| d.code == "IPC-001"));
        assert!(has_errors(&diagnostics));
    }

    #[test]
    fn test_ipc_001_no_conflict_when_default_sharing() {
        let config = IpcConfig {
            exports: Some(IpcExportsConfig {
                name: Some("my-service".to_string()),
                methods: vec![],
                sharing: IpcSharingConfig::default(),
            }),
            imports: HashMap::new(),
        };

        let diagnostics = validate_ipc(&config, &toml_with_lifecycle(), Path::new("/tmp"), &[]);
        assert!(!diagnostics.iter().any(|d| d.code == "IPC-001"));
    }

    #[test]
    fn test_ipc_002_reserved_prefix_in_service_name() {
        let config = empty_config();
        let services = vec!["_ipc_custom".to_string()];
        let diagnostics = validate_ipc(&config, &empty_toml(), Path::new("/tmp"), &services);
        assert!(diagnostics.iter().any(|d| d.code == "IPC-002"));
        assert!(has_errors(&diagnostics));
    }

    #[test]
    fn test_ipc_002_reserved_prefix_in_export_name() {
        let config = IpcConfig {
            exports: Some(IpcExportsConfig {
                name: Some("_ipc_my_service".to_string()),
                methods: vec![],
                sharing: IpcSharingConfig::default(),
            }),
            imports: HashMap::new(),
        };

        let diagnostics = validate_ipc(&config, &empty_toml(), Path::new("/tmp"), &[]);
        assert!(diagnostics.iter().any(|d| d.code == "IPC-002"));
    }

    #[test]
    fn test_ipc_002_reserved_prefix_in_import_name() {
        let mut imports = HashMap::new();
        imports.insert(
            "_setup_db".to_string(),
            IpcImportConfig {
                from: "db-service".to_string(),
                activation: ActivationMode::Eager,
                optional: false,
            },
        );

        let config = IpcConfig {
            exports: None,
            imports,
        };

        let diagnostics = validate_ipc(&config, &empty_toml(), Path::new("/tmp"), &[]);
        assert!(diagnostics.iter().any(|d| d.code == "IPC-002"));
    }

    #[test]
    fn test_ipc_005_remote_capability_warning() {
        let config = IpcConfig {
            exports: Some(IpcExportsConfig {
                name: Some("my-service".to_string()),
                methods: vec![IpcMethodDescriptor {
                    name: "fetch_data".to_string(),
                    description: "Fetches data from remote API".to_string(),
                    input_schema: None,
                    output_schema: None,
                }],
                sharing: IpcSharingConfig::default(),
            }),
            imports: HashMap::new(),
        };

        let diagnostics = validate_ipc(&config, &empty_toml(), Path::new("/tmp"), &[]);
        assert!(diagnostics.iter().any(|d| d.code == "IPC-005"));
        // Should be warning, not error
        assert!(!has_errors(&diagnostics));
    }

    #[test]
    fn test_ipc_006_missing_relative_import() {
        let mut imports = HashMap::new();
        imports.insert(
            "my-lib".to_string(),
            IpcImportConfig {
                from: "./nonexistent-lib".to_string(),
                activation: ActivationMode::Eager,
                optional: false,
            },
        );

        let config = IpcConfig {
            exports: None,
            imports,
        };

        let diagnostics = validate_ipc(&config, &empty_toml(), Path::new("/tmp/test-capsule"), &[]);
        assert!(diagnostics.iter().any(|d| d.code == "IPC-006"));
        assert!(has_errors(&diagnostics));
    }

    #[test]
    fn test_ipc_006_optional_import_is_warning() {
        let mut imports = HashMap::new();
        imports.insert(
            "analytics".to_string(),
            IpcImportConfig {
                from: "./optional-analytics".to_string(),
                activation: ActivationMode::Lazy,
                optional: true,
            },
        );

        let config = IpcConfig {
            exports: None,
            imports,
        };

        let diagnostics = validate_ipc(&config, &empty_toml(), Path::new("/tmp/test-capsule"), &[]);
        let ipc006 = diagnostics.iter().find(|d| d.code == "IPC-006");
        assert!(ipc006.is_some());
        assert_eq!(ipc006.unwrap().severity, Severity::Warning);
        assert!(!has_errors(&diagnostics));
    }

    #[test]
    fn test_ipc_007_empty_exports_warning() {
        let config = IpcConfig {
            exports: Some(IpcExportsConfig {
                name: Some("empty-service".to_string()),
                methods: vec![],
                sharing: IpcSharingConfig::default(),
            }),
            imports: HashMap::new(),
        };

        let diagnostics = validate_ipc(&config, &empty_toml(), Path::new("/tmp"), &[]);
        assert!(diagnostics.iter().any(|d| d.code == "IPC-007"));
        assert!(!has_errors(&diagnostics));
    }

    #[test]
    fn test_ipc_008_missing_schema_file_is_error() {
        let config = IpcConfig {
            exports: Some(IpcExportsConfig {
                name: Some("schema-service".to_string()),
                methods: vec![IpcMethodDescriptor {
                    name: "ping".to_string(),
                    description: String::new(),
                    input_schema: Some("schemas/missing.json".to_string()),
                    output_schema: None,
                }],
                sharing: IpcSharingConfig::default(),
            }),
            imports: HashMap::new(),
        };

        let diagnostics = validate_ipc(&config, &empty_toml(), Path::new("/tmp"), &[]);
        assert!(diagnostics.iter().any(|d| d.code == "IPC-008"));
        assert!(has_errors(&diagnostics));
    }

    #[test]
    fn test_validate_manifest_reports_compilable_schema() {
        let temp = tempfile::tempdir().unwrap();
        let schema_dir = temp.path().join("schemas");
        std::fs::create_dir_all(&schema_dir).unwrap();
        std::fs::write(
            schema_dir.join("input.json"),
            r#"{"type":"object","properties":{"name":{"type":"string"}}}"#,
        )
        .unwrap();

        let manifest: toml::Value = toml::from_str(
            r#"
            [ipc.exports]
            name = "schema-service"

            [[ipc.exports.methods]]
            name = "ping"
            input_schema = "schemas/input.json"
            "#,
        )
        .unwrap();

        let diagnostics = validate_manifest(&manifest, temp.path()).unwrap();
        assert!(
            diagnostics.is_empty(),
            "expected no diagnostics, got: {}",
            format_diagnostics(&diagnostics)
        );
    }

    #[test]
    fn test_valid_service_names_pass() {
        let config = empty_config();
        let services = vec![
            "web-frontend".to_string(),
            "api-server".to_string(),
            "database".to_string(),
        ];
        let diagnostics = validate_ipc(&config, &empty_toml(), Path::new("/tmp"), &services);
        assert!(
            diagnostics.is_empty(),
            "Valid service names should produce no diagnostics"
        );
    }

    #[test]
    fn test_format_diagnostics_empty() {
        let output = format_diagnostics(&[]);
        assert!(output.contains("valid"));
    }

    #[test]
    fn test_format_diagnostics_with_errors() {
        let diags = vec![IpcDiagnostic {
            severity: Severity::Error,
            code: "IPC-001",
            message: "test error".to_string(),
            location: "[ipc]".to_string(),
            hint: "fix it".to_string(),
        }];
        let output = format_diagnostics(&diags);
        assert!(output.contains("1 error(s)"));
        assert!(output.contains("IPC-001"));
    }

    #[test]
    fn test_has_errors() {
        assert!(!has_errors(&[]));
        assert!(!has_errors(&[IpcDiagnostic {
            severity: Severity::Warning,
            code: "IPC-005",
            message: "warn".to_string(),
            location: "".to_string(),
            hint: "".to_string(),
        }]));
        assert!(has_errors(&[IpcDiagnostic {
            severity: Severity::Error,
            code: "IPC-001",
            message: "err".to_string(),
            location: "".to_string(),
            hint: "".to_string(),
        }]));
    }
}
