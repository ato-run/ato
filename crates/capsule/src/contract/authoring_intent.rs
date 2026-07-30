//! Source-agnostic Program Intent authoring contract.
//!
//! This module is deliberately pure. It validates and canonicalizes an
//! authoring draft, but it does not resolve tools, write `capsule.lock`, build
//! bytes, or mint an Execution Identity.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::types::manifest_v1::{BuildStepV1, BuildV1, CapsuleManifestV1, ConfigKindV1, SealAtV1};

pub const PROGRAM_INTENT_DRAFT_V1_SCHEMA: &str = "ato.program-intent-draft/v1";
pub const NORMALIZED_PROGRAM_INTENT_V1_SCHEMA: &str = "ato.normalized-program-intent/v1";
const NORMALIZED_PROGRAM_INTENT_V1_DOMAIN: &[u8] = b"ato.normalized-program-intent/v1";
const RESOLUTION_LOCK_V1_DOMAIN: &[u8] = b"ato.resolution-lock/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgramIntentOrigin {
    Inference,
    ManualSetup,
    CapsuleManifest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramIntentDraftV1 {
    pub schema: String,
    pub origin: ProgramIntentOrigin,
    #[serde(default)]
    pub toolchains: Vec<ToolchainRequirementV1>,
    #[serde(default)]
    pub build_steps: Vec<ProgramCommandDraftV1>,
    pub launch: ProgramCommandDraftV1,
    pub readiness: ReadinessIntentV1,
    #[serde(default)]
    pub build_output_roots: Vec<WorkspacePathV1>,
    #[serde(default)]
    pub bindings: Vec<BindingRequirementV1>,
    #[serde(default)]
    pub unresolved: Vec<UnresolvedIntentItemV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolchainRequirementV1 {
    pub name: String,
    pub version_constraint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ProgramCommandDraftV1 {
    Argv {
        argv: Vec<String>,
        cwd: WorkspacePathV1,
        #[serde(default)]
        requested_environment: Vec<String>,
        #[serde(default)]
        required_tools: Vec<String>,
    },
    ShellEscapeHatch {
        interpreter_argv: Vec<String>,
        script: String,
        justification: String,
        cwd: WorkspacePathV1,
        #[serde(default)]
        requested_environment: Vec<String>,
        #[serde(default)]
        required_tools: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct WorkspacePathV1(String);

impl WorkspacePathV1 {
    pub fn parse(value: impl Into<String>) -> Result<Self, ProgramIntentError> {
        let value = value.into();
        validate_workspace_path(&value)?;
        Ok(Self(value))
    }

    pub fn root() -> Self {
        Self(".".to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for WorkspacePathV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ReadinessIntentV1 {
    ProcessRunning {
        minimum_stable_seconds: u32,
    },
    Tcp {
        port: u16,
        timeout_seconds: u32,
    },
    Http {
        port: u16,
        path: String,
        timeout_seconds: u32,
    },
    Exec {
        argv: Vec<String>,
        timeout_seconds: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingKindV1 {
    Secret,
    State,
    Identity,
    ExternalService,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindingRequirementV1 {
    pub name: String,
    pub kind: BindingKindV1,
    pub required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnresolvedIntentKindV1 {
    NeedsReview,
    Unreproducible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnresolvedIntentItemV1 {
    pub kind: UnresolvedIntentKindV1,
    pub code: String,
    pub redacted_detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedProgramIntentV1 {
    pub schema: String,
    pub toolchains: Vec<ToolchainRequirementV1>,
    pub build_steps: Vec<NormalizedProgramCommandV1>,
    pub launch: NormalizedProgramCommandV1,
    pub readiness: ReadinessIntentV1,
    pub build_output_roots: Vec<WorkspacePathV1>,
    pub bindings: Vec<BindingRequirementV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedProgramCommandV1 {
    pub argv: Vec<String>,
    pub cwd: WorkspacePathV1,
    pub requested_environment: Vec<String>,
    pub required_tools: Vec<String>,
    pub explicit_shell_escape: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedProgramIntentEnvelopeV1 {
    pub intent: NormalizedProgramIntentV1,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProgramIntentError {
    #[error("program intent schema must be ato.program-intent-draft/v1")]
    InvalidSchema,
    #[error("{field} must not be empty")]
    Empty { field: &'static str },
    #[error("{field} contains a NUL byte")]
    Nul { field: &'static str },
    #[error("workspace path is unsafe: {0}")]
    UnsafeWorkspacePath(String),
    #[error("environment name is invalid: {0}")]
    InvalidEnvironmentName(String),
    #[error("list must be sorted and duplicate-free after normalization: {0}")]
    Duplicate(String),
    #[error("readiness is invalid: {0}")]
    InvalidReadiness(String),
    #[error("program intent has unresolved item {code} ({kind:?})")]
    Unresolved {
        code: String,
        kind: UnresolvedIntentKindV1,
    },
    #[error("explicit shell escape hatch requires a non-empty justification")]
    ShellJustificationRequired,
    #[error("failed to canonicalize normalized Program Intent: {0}")]
    Canonicalization(String),
}

pub fn normalize_program_intent(
    draft: ProgramIntentDraftV1,
) -> Result<NormalizedProgramIntentEnvelopeV1, ProgramIntentError> {
    if draft.schema != PROGRAM_INTENT_DRAFT_V1_SCHEMA {
        return Err(ProgramIntentError::InvalidSchema);
    }
    if let Some(item) = draft.unresolved.first() {
        return Err(ProgramIntentError::Unresolved {
            code: item.code.clone(),
            kind: item.kind,
        });
    }

    let mut toolchains = draft.toolchains;
    for tool in &toolchains {
        validate_non_empty("toolchains[].name", &tool.name)?;
        validate_non_empty("toolchains[].version_constraint", &tool.version_constraint)?;
    }
    toolchains
        .sort_by(|a, b| (&a.name, &a.version_constraint).cmp(&(&b.name, &b.version_constraint)));
    reject_duplicate_by(&toolchains, |item| item.name.as_str(), "toolchains[].name")?;

    let build_steps = draft
        .build_steps
        .into_iter()
        .map(normalize_command)
        .collect::<Result<Vec<_>, _>>()?;
    let launch = normalize_command(draft.launch)?;
    validate_readiness(&draft.readiness)?;

    let mut output_roots = draft.build_output_roots;
    for path in &output_roots {
        validate_workspace_path(path.as_str())?;
    }
    output_roots.sort();
    if output_roots.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(ProgramIntentError::Duplicate(
            "build_output_roots".to_string(),
        ));
    }

    let mut bindings = draft.bindings;
    for binding in &bindings {
        validate_env_name(&binding.name)?;
    }
    bindings.sort();
    if bindings.windows(2).any(|pair| pair[0].name == pair[1].name) {
        return Err(ProgramIntentError::Duplicate("bindings[].name".to_string()));
    }

    let intent = NormalizedProgramIntentV1 {
        schema: NORMALIZED_PROGRAM_INTENT_V1_SCHEMA.to_string(),
        toolchains,
        build_steps,
        launch,
        readiness: draft.readiness,
        build_output_roots: output_roots,
        bindings,
    };
    let canonical = serde_jcs::to_vec(&intent)
        .map_err(|error| ProgramIntentError::Canonicalization(error.to_string()))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(NORMALIZED_PROGRAM_INTENT_V1_DOMAIN);
    hasher.update(&[0]);
    hasher.update(&canonical);
    let digest = format!("blake3:{}", hasher.finalize().to_hex());
    Ok(NormalizedProgramIntentEnvelopeV1 { intent, digest })
}

/// Digest resolver output as canonical JSON.
///
/// The lock is output, never author input. Canonicalizing the parsed JSON keeps
/// its identity independent of writer whitespace while still binding every
/// resolver-produced value.
pub fn resolution_lock_digest(lock_json: &[u8]) -> Result<String, ProgramIntentError> {
    let value: serde_json::Value = serde_json::from_slice(lock_json)
        .map_err(|error| ProgramIntentError::Canonicalization(error.to_string()))?;
    let canonical = serde_jcs::to_vec(&value)
        .map_err(|error| ProgramIntentError::Canonicalization(error.to_string()))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(RESOLUTION_LOCK_V1_DOMAIN);
    hasher.update(&[0]);
    hasher.update(&canonical);
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

/// Adapt the already-strict v1 manifest into the same producer used by
/// inference and manual setup. This conversion never resolves a lock and never
/// issues an Execution Identity.
pub fn draft_from_capsule_manifest_v1(
    manifest: &CapsuleManifestV1,
) -> Result<ProgramIntentDraftV1, ProgramIntentError> {
    let command = |argv: &[String]| ProgramCommandDraftV1::Argv {
        argv: argv.to_vec(),
        cwd: WorkspacePathV1::root(),
        requested_environment: Vec::new(),
        required_tools: Vec::new(),
    };
    let toolchains = manifest
        .tools
        .iter()
        .map(|(name, version_constraint)| ToolchainRequirementV1 {
            name: name.clone(),
            version_constraint: version_constraint.clone(),
        })
        .collect();
    let build_steps = manifest
        .build
        .as_ref()
        .map(|build| {
            build
                .steps
                .iter()
                .map(|step| command(&step.command))
                .collect()
        })
        .unwrap_or_default();
    let bindings = manifest
        .config
        .iter()
        .map(|(name, field)| BindingRequirementV1 {
            name: name.clone(),
            kind: match field.kind {
                ConfigKindV1::Secret => BindingKindV1::Secret,
                ConfigKindV1::String | ConfigKindV1::Number => BindingKindV1::ExternalService,
            },
            required: field.required,
        })
        .collect();
    let readiness = match &manifest.web {
        Some(web) => ReadinessIntentV1::Http {
            port: web.port,
            path: "/".to_string(),
            timeout_seconds: 60,
        },
        None => ReadinessIntentV1::ProcessRunning {
            minimum_stable_seconds: 2,
        },
    };
    let mut draft = ProgramIntentDraftV1 {
        schema: PROGRAM_INTENT_DRAFT_V1_SCHEMA.to_string(),
        origin: ProgramIntentOrigin::CapsuleManifest,
        toolchains,
        build_steps,
        launch: command(&manifest.run.command),
        readiness,
        build_output_roots: Vec::new(),
        bindings,
        unresolved: Vec::new(),
    };
    // Seal acceptance is not readiness. Preserve its exact argv as an explicit
    // validation step only when a later materialization adapter asks for it;
    // never silently reinterpret it here.
    if let Some(SealAtV1 { command, .. }) = &manifest.seal_at {
        validate_argv("seal_at.command", command)?;
    }
    // Reuse the common validator before returning an adapter-produced draft.
    normalize_program_intent(draft.clone())?;
    draft.origin = ProgramIntentOrigin::CapsuleManifest;
    Ok(draft)
}

/// Materialize the supported normalized subset as a strict v1 manifest.
/// Resolution still happens later and creates `capsule.lock` as output.
pub fn to_capsule_manifest_v1(
    name: String,
    version: String,
    normalized: &NormalizedProgramIntentV1,
    seal_at: SealAtV1,
) -> Result<CapsuleManifestV1, ProgramIntentError> {
    validate_non_empty("name", &name)?;
    validate_non_empty("version", &version)?;
    if normalized
        .build_steps
        .iter()
        .chain(std::iter::once(&normalized.launch))
        .any(|command| {
            command.cwd != WorkspacePathV1::root() || !command.requested_environment.is_empty()
        })
    {
        return Err(ProgramIntentError::Unresolved {
            code: "manifest_v1_subset_cannot_encode_cwd_or_requested_environment".to_string(),
            kind: UnresolvedIntentKindV1::NeedsReview,
        });
    }
    let build = (!normalized.build_steps.is_empty()).then(|| BuildV1 {
        steps: normalized
            .build_steps
            .iter()
            .map(|command| BuildStepV1 {
                command: command.argv.clone(),
            })
            .collect(),
    });
    Ok(CapsuleManifestV1 {
        schema_version: "1".to_string(),
        name,
        version,
        source: Default::default(),
        metadata: Default::default(),
        tools: normalized
            .toolchains
            .iter()
            .map(|tool| (tool.name.clone(), tool.version_constraint.clone()))
            .collect(),
        build,
        run: crate::types::manifest_v1::RunV1 {
            command: normalized.launch.argv.clone(),
        },
        web: match normalized.readiness {
            ReadinessIntentV1::Tcp { port, .. } | ReadinessIntentV1::Http { port, .. } => {
                Some(crate::types::manifest_v1::WebV1 {
                    port,
                    bind: "0.0.0.0".to_string(),
                })
            }
            _ => None,
        },
        seal_at: Some(seal_at),
        env: Default::default(),
        config: Default::default(),
        state: Default::default(),
    })
}

fn normalize_command(
    command: ProgramCommandDraftV1,
) -> Result<NormalizedProgramCommandV1, ProgramIntentError> {
    let (argv, cwd, mut requested_environment, mut required_tools, explicit_shell_escape) =
        match command {
            ProgramCommandDraftV1::Argv {
                argv,
                cwd,
                requested_environment,
                required_tools,
            } => (argv, cwd, requested_environment, required_tools, false),
            ProgramCommandDraftV1::ShellEscapeHatch {
                mut interpreter_argv,
                script,
                justification,
                cwd,
                requested_environment,
                required_tools,
            } => {
                validate_non_empty("shell_escape_hatch.justification", &justification)
                    .map_err(|_| ProgramIntentError::ShellJustificationRequired)?;
                validate_argv("shell_escape_hatch.interpreter_argv", &interpreter_argv)?;
                validate_non_empty("shell_escape_hatch.script", &script)?;
                interpreter_argv.push(script);
                (
                    interpreter_argv,
                    cwd,
                    requested_environment,
                    required_tools,
                    true,
                )
            }
        };
    validate_argv("command.argv", &argv)?;
    validate_workspace_path(cwd.as_str())?;
    for name in &requested_environment {
        validate_env_name(name)?;
    }
    requested_environment.sort();
    reject_duplicates(&requested_environment, "command.requested_environment")?;
    for tool in &required_tools {
        validate_non_empty("command.required_tools[]", tool)?;
    }
    required_tools.sort();
    reject_duplicates(&required_tools, "command.required_tools")?;
    Ok(NormalizedProgramCommandV1 {
        argv,
        cwd,
        requested_environment,
        required_tools,
        explicit_shell_escape,
    })
}

fn validate_argv(field: &'static str, argv: &[String]) -> Result<(), ProgramIntentError> {
    if argv.is_empty() || argv[0].trim().is_empty() {
        return Err(ProgramIntentError::Empty { field });
    }
    if argv.iter().any(|argument| argument.contains('\0')) {
        return Err(ProgramIntentError::Nul { field });
    }
    Ok(())
}

fn validate_readiness(readiness: &ReadinessIntentV1) -> Result<(), ProgramIntentError> {
    match readiness {
        ReadinessIntentV1::ProcessRunning {
            minimum_stable_seconds,
        } if *minimum_stable_seconds == 0 => Err(ProgramIntentError::InvalidReadiness(
            "minimum_stable_seconds must be positive".to_string(),
        )),
        ReadinessIntentV1::Tcp {
            port,
            timeout_seconds,
        }
        | ReadinessIntentV1::Http {
            port,
            timeout_seconds,
            ..
        } if *port == 0 || *timeout_seconds == 0 => Err(ProgramIntentError::InvalidReadiness(
            "port and timeout must be positive".to_string(),
        )),
        ReadinessIntentV1::Http { path, .. } if !path.starts_with('/') || path.contains('\0') => {
            Err(ProgramIntentError::InvalidReadiness(
                "HTTP path must be absolute and contain no NUL".to_string(),
            ))
        }
        ReadinessIntentV1::Exec {
            argv,
            timeout_seconds,
        } => {
            validate_argv("readiness.exec.argv", argv)?;
            if *timeout_seconds == 0 {
                return Err(ProgramIntentError::InvalidReadiness(
                    "exec timeout must be positive".to_string(),
                ));
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn validate_workspace_path(value: &str) -> Result<(), ProgramIntentError> {
    if value.is_empty()
        || value.starts_with('/')
        || value.contains('\\')
        || value.chars().any(char::is_control)
        || value
            .split('/')
            .any(|segment| segment.is_empty() || segment == "..")
        || (value != "." && value.ends_with('/'))
    {
        return Err(ProgramIntentError::UnsafeWorkspacePath(value.to_string()));
    }
    Ok(())
}

fn validate_non_empty(field: &'static str, value: &str) -> Result<(), ProgramIntentError> {
    if value.trim().is_empty() {
        return Err(ProgramIntentError::Empty { field });
    }
    if value.contains('\0') {
        return Err(ProgramIntentError::Nul { field });
    }
    Ok(())
}

fn validate_env_name(value: &str) -> Result<(), ProgramIntentError> {
    let mut chars = value.chars();
    if !matches!(chars.next(), Some(first) if first.is_ascii_alphabetic() || first == '_')
        || !chars.all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return Err(ProgramIntentError::InvalidEnvironmentName(
            value.to_string(),
        ));
    }
    Ok(())
}

fn reject_duplicates(values: &[String], field: &'static str) -> Result<(), ProgramIntentError> {
    if values.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(ProgramIntentError::Duplicate(field.to_string()));
    }
    Ok(())
}

fn reject_duplicate_by<'a, T>(
    values: &'a [T],
    key: impl Fn(&'a T) -> &'a str,
    field: &'static str,
) -> Result<(), ProgramIntentError> {
    let mut seen = BTreeSet::new();
    if values.iter().any(|value| !seen.insert(key(value))) {
        return Err(ProgramIntentError::Duplicate(field.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> ProgramCommandDraftV1 {
        ProgramCommandDraftV1::Argv {
            argv: parts.iter().map(|part| (*part).to_string()).collect(),
            cwd: WorkspacePathV1::root(),
            requested_environment: Vec::new(),
            required_tools: Vec::new(),
        }
    }

    fn draft() -> ProgramIntentDraftV1 {
        ProgramIntentDraftV1 {
            schema: PROGRAM_INTENT_DRAFT_V1_SCHEMA.to_string(),
            origin: ProgramIntentOrigin::Inference,
            toolchains: vec![ToolchainRequirementV1 {
                name: "node".to_string(),
                version_constraint: "20".to_string(),
            }],
            build_steps: vec![argv(&["pnpm", "install"]), argv(&["pnpm", "build"])],
            launch: argv(&["node", "server.js", "--label=a b"]),
            readiness: ReadinessIntentV1::Http {
                port: 8000,
                path: "/health".to_string(),
                timeout_seconds: 60,
            },
            build_output_roots: vec![WorkspacePathV1::parse("dist").expect("path")],
            bindings: Vec::new(),
            unresolved: Vec::new(),
        }
    }

    #[test]
    fn normalization_preserves_argv_boundaries_and_ordered_steps() {
        let normalized = normalize_program_intent(draft()).expect("normalizes");
        assert_eq!(normalized.intent.build_steps[0].argv, ["pnpm", "install"]);
        assert_eq!(normalized.intent.build_steps[1].argv, ["pnpm", "build"]);
        assert_eq!(
            normalized.intent.launch.argv,
            ["node", "server.js", "--label=a b"]
        );
        assert!(normalized.digest.starts_with("blake3:"));
    }

    #[test]
    fn explicit_shell_escape_is_visible_in_normalized_argv() {
        let mut input = draft();
        input.build_steps = vec![ProgramCommandDraftV1::ShellEscapeHatch {
            interpreter_argv: vec!["sh".to_string(), "-lc".to_string()],
            script: "pnpm install && pnpm build".to_string(),
            justification: "legacy package lifecycle requires a shell".to_string(),
            cwd: WorkspacePathV1::root(),
            requested_environment: Vec::new(),
            required_tools: vec!["sh".to_string()],
        }];
        let normalized = normalize_program_intent(input).expect("normalizes");
        assert!(normalized.intent.build_steps[0].explicit_shell_escape);
        assert_eq!(
            normalized.intent.build_steps[0].argv,
            ["sh", "-lc", "pnpm install && pnpm build"]
        );
    }

    #[test]
    fn unresolved_manual_action_blocks_normalization() {
        let mut input = draft();
        input.unresolved.push(UnresolvedIntentItemV1 {
            kind: UnresolvedIntentKindV1::Unreproducible,
            code: "host_binary".to_string(),
            redacted_detail: "binary exists only on the host".to_string(),
        });
        assert!(matches!(
            normalize_program_intent(input),
            Err(ProgramIntentError::Unresolved { code, .. }) if code == "host_binary"
        ));
    }

    #[test]
    fn unsafe_overlay_style_path_is_rejected() {
        assert!(matches!(
            WorkspacePathV1::parse("../host"),
            Err(ProgramIntentError::UnsafeWorkspacePath(_))
        ));
    }

    #[test]
    fn manifest_and_inference_use_the_same_normalizer() {
        let manifest = CapsuleManifestV1::from_toml(
            r#"
schema_version = "1"
name = "example"
version = "1.0.0"
[tools]
node = "20"
[[build.steps]]
command = ["pnpm", "build"]
[run]
command = ["node", "server.js"]
[web]
port = 8000
bind = "0.0.0.0"
[seal_at]
command = ["node", "verify.js"]
"#,
        )
        .expect("manifest");
        let from_manifest =
            normalize_program_intent(draft_from_capsule_manifest_v1(&manifest).expect("draft"))
                .expect("normalizes");
        assert_eq!(from_manifest.intent.launch.argv, ["node", "server.js"]);
        assert_eq!(from_manifest.intent.toolchains[0].name, "node");
        assert_eq!(
            from_manifest.intent.readiness,
            ReadinessIntentV1::Http {
                port: 8000,
                path: "/".to_string(),
                timeout_seconds: 60,
            }
        );
    }

    #[test]
    fn resolution_lock_digest_ignores_formatting_but_binds_values() {
        let compact = br#"{"runtime":{"ref":"python:3.11"},"version":1}"#;
        let formatted = b"{\n  \"version\": 1,\n  \"runtime\": { \"ref\": \"python:3.11\" }\n}\n";
        assert_eq!(
            resolution_lock_digest(compact).expect("compact"),
            resolution_lock_digest(formatted).expect("formatted"),
        );
        assert_ne!(
            resolution_lock_digest(compact).expect("compact"),
            resolution_lock_digest(br#"{"runtime":{"ref":"python:3.12"},"version":1}"#)
                .expect("changed"),
        );
    }

    #[test]
    fn secret_binding_has_name_only() {
        let json = serde_json::to_string(&BindingRequirementV1 {
            name: "API_TOKEN".to_string(),
            kind: BindingKindV1::Secret,
            required: true,
        })
        .expect("json");
        assert_eq!(
            json,
            r#"{"name":"API_TOKEN","kind":"secret","required":true}"#
        );
    }
}
