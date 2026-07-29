//! Typed manual-setup journal and pure intent update.
//!
//! PTY text is intentionally absent. Only observer-produced typed events may
//! influence Program Intent.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::authoring_intent::{
    BindingKindV1, BindingRequirementV1, ProgramCommandDraftV1, ProgramIntentDraftV1,
    ProgramIntentError, UnresolvedIntentItemV1, UnresolvedIntentKindV1, WorkspacePathV1,
};

pub const SETUP_JOURNAL_V1_SCHEMA: &str = "ato.setup-journal/v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupJournalV1 {
    pub schema: String,
    pub authoring_session_id: String,
    pub events: Vec<SetupEventV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupEventV1 {
    pub sequence: u64,
    pub command_argv: Vec<String>,
    pub cwd: WorkspacePathV1,
    pub exit_code: i32,
    pub started_at: String,
    pub finished_at: String,
    #[serde(default)]
    pub requested_environment_names: Vec<String>,
    #[serde(default)]
    pub network_artifacts: Vec<NetworkArtifactReferenceV1>,
    #[serde(default)]
    pub filesystem_changes: Vec<FilesystemChangeV1>,
    #[serde(default)]
    pub resolved_tools: Vec<ResolvedToolIdentityV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkArtifactReferenceV1 {
    pub url_origin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_digest: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesystemChangeKindV1 {
    Created,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemChangeV1 {
    pub path: WorkspacePathV1,
    pub kind: FilesystemChangeKindV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedToolIdentityV1 {
    pub name: String,
    pub version: String,
    pub artifact_digest: String,
    pub provided_by_builder: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupActionClassV1 {
    DeclarativeStep,
    SourceOverlay,
    BindingRequirement,
    UnreproducibleAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupActionProposalV1 {
    pub sequence: u64,
    pub classification: SetupActionClassV1,
    pub redacted_summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupJournalAnalysisV1 {
    pub proposals: Vec<SetupActionProposalV1>,
    pub updated_draft: ProgramIntentDraftV1,
    pub publish_blocked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SetupJournalError {
    #[error("setup journal schema must be ato.setup-journal/v1")]
    InvalidSchema,
    #[error("authoring_session_id must not be empty")]
    EmptySession,
    #[error("setup event sequences must be strictly increasing")]
    NonMonotonicSequence,
    #[error("setup event {sequence} has invalid argv")]
    InvalidArgv { sequence: u64 },
    #[error("setup event {sequence} contains a possible secret value")]
    PossibleSecretValue { sequence: u64 },
    #[error("program intent update failed: {0}")]
    Intent(#[from] ProgramIntentError),
}

pub fn apply_setup_journal(
    mut draft: ProgramIntentDraftV1,
    journal: &SetupJournalV1,
) -> Result<SetupJournalAnalysisV1, SetupJournalError> {
    if journal.schema != SETUP_JOURNAL_V1_SCHEMA {
        return Err(SetupJournalError::InvalidSchema);
    }
    if journal.authoring_session_id.trim().is_empty() {
        return Err(SetupJournalError::EmptySession);
    }
    if journal
        .events
        .windows(2)
        .any(|pair| pair[0].sequence >= pair[1].sequence)
    {
        return Err(SetupJournalError::NonMonotonicSequence);
    }

    let mut proposals = Vec::new();
    for event in &journal.events {
        validate_event(event)?;
        if event.exit_code != 0 {
            proposals.push(proposal(
                event,
                SetupActionClassV1::UnreproducibleAction,
                "failed command requires review",
            ));
            draft.unresolved.push(unresolved(
                "failed_setup_command",
                UnresolvedIntentKindV1::NeedsReview,
            ));
            continue;
        }

        if is_privileged_or_host_specific(event) {
            proposals.push(proposal(
                event,
                SetupActionClassV1::UnreproducibleAction,
                "privileged or host-specific command",
            ));
            draft.unresolved.push(unresolved(
                "privileged_or_host_specific_action",
                UnresolvedIntentKindV1::Unreproducible,
            ));
            continue;
        }

        if event
            .network_artifacts
            .iter()
            .any(|artifact| artifact.content_digest.is_none())
        {
            proposals.push(proposal(
                event,
                SetupActionClassV1::UnreproducibleAction,
                "network download is not content-pinned",
            ));
            draft.unresolved.push(unresolved(
                "unpinned_network_artifact",
                UnresolvedIntentKindV1::Unreproducible,
            ));
            continue;
        }

        let sensitive_paths = event
            .filesystem_changes
            .iter()
            .filter(|change| looks_sensitive_path(change.path.as_str()))
            .collect::<Vec<_>>();
        if !sensitive_paths.is_empty() {
            proposals.push(proposal(
                event,
                SetupActionClassV1::BindingRequirement,
                "sensitive file converted to a binding requirement",
            ));
            for change in sensitive_paths {
                let name = binding_name_for_path(change.path.as_str());
                if !draft.bindings.iter().any(|binding| binding.name == name) {
                    draft.bindings.push(BindingRequirementV1 {
                        name,
                        kind: BindingKindV1::Secret,
                        required: true,
                    });
                }
            }
            draft.unresolved.push(unresolved(
                "sensitive_file_observed",
                UnresolvedIntentKindV1::NeedsReview,
            ));
            continue;
        }

        if is_launch_command(event) {
            proposals.push(proposal(
                event,
                SetupActionClassV1::DeclarativeStep,
                "launch command",
            ));
            draft.launch = command_from_event(event);
        } else if is_declarative_build_command(event) {
            proposals.push(proposal(
                event,
                SetupActionClassV1::DeclarativeStep,
                "build or dependency step",
            ));
            draft.build_steps.push(command_from_event(event));
        } else if !event.filesystem_changes.is_empty() {
            proposals.push(proposal(
                event,
                SetupActionClassV1::SourceOverlay,
                "source overlay changes",
            ));
            // Overlay bytes are captured by the builder. The intent records
            // only the fact that an overlay needs review, never file content.
            draft.unresolved.push(unresolved(
                "source_overlay_requires_review",
                UnresolvedIntentKindV1::NeedsReview,
            ));
        } else {
            proposals.push(proposal(
                event,
                SetupActionClassV1::UnreproducibleAction,
                "command semantics are ambiguous",
            ));
            draft.unresolved.push(unresolved(
                "ambiguous_setup_action",
                UnresolvedIntentKindV1::NeedsReview,
            ));
        }
    }

    let publish_blocked = !draft.unresolved.is_empty();
    Ok(SetupJournalAnalysisV1 {
        proposals,
        updated_draft: draft,
        publish_blocked,
    })
}

fn validate_event(event: &SetupEventV1) -> Result<(), SetupJournalError> {
    if event.command_argv.is_empty()
        || event.command_argv[0].trim().is_empty()
        || event.command_argv.iter().any(|arg| arg.contains('\0'))
    {
        return Err(SetupJournalError::InvalidArgv {
            sequence: event.sequence,
        });
    }
    // The observer must redact values before ingestion. Reject common
    // value-bearing command spellings rather than trying to sanitize them here.
    if event.command_argv.iter().any(|arg| {
        let lower = arg.to_ascii_lowercase();
        (lower.starts_with("--token=")
            || lower.starts_with("--password=")
            || lower.starts_with("--secret=")
            || lower.starts_with("authorization:"))
            && !lower.ends_with("[redacted]")
    }) {
        return Err(SetupJournalError::PossibleSecretValue {
            sequence: event.sequence,
        });
    }
    Ok(())
}

fn command_from_event(event: &SetupEventV1) -> ProgramCommandDraftV1 {
    ProgramCommandDraftV1::Argv {
        argv: event.command_argv.clone(),
        cwd: event.cwd.clone(),
        requested_environment: event.requested_environment_names.clone(),
        required_tools: event
            .resolved_tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect(),
    }
}

fn is_privileged_or_host_specific(event: &SetupEventV1) -> bool {
    matches!(
        event.command_argv[0].as_str(),
        "sudo" | "su" | "doas" | "launchctl" | "open"
    ) || event
        .resolved_tools
        .iter()
        .any(|tool| !tool.provided_by_builder)
}

fn is_declarative_build_command(event: &SetupEventV1) -> bool {
    let argv = &event.command_argv;
    matches!(
        argv.first().map(String::as_str),
        Some("npm" | "pnpm" | "yarn" | "bun" | "pip" | "pip3" | "uv" | "cargo" | "go")
    ) && argv.get(1).is_some_and(|subcommand| {
        matches!(
            subcommand.as_str(),
            "install" | "ci" | "sync" | "build" | "fetch"
        )
    })
}

fn is_launch_command(event: &SetupEventV1) -> bool {
    let argv = &event.command_argv;
    argv.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "uvicorn" | "gunicorn" | "serve" | "start" | "dev"
        )
    }) || matches!(
        argv.first().map(String::as_str),
        Some("node" | "python" | "python3" | "deno")
    ) && !is_declarative_build_command(event)
}

fn looks_sensitive_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower == ".env"
        || lower.ends_with("/.env")
        || lower.contains("/.ssh/")
        || lower.ends_with(".pem")
        || lower.ends_with(".key")
        || lower.ends_with("credentials")
        || lower.contains(".aws/")
        || lower.contains(".config/gcloud/")
}

fn binding_name_for_path(path: &str) -> String {
    let mut name = String::from("FILE_");
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() {
            name.push((byte as char).to_ascii_uppercase());
        } else if !name.ends_with('_') {
            name.push('_');
        }
    }
    name.trim_end_matches('_').to_string()
}

fn proposal(
    event: &SetupEventV1,
    classification: SetupActionClassV1,
    summary: &str,
) -> SetupActionProposalV1 {
    SetupActionProposalV1 {
        sequence: event.sequence,
        classification,
        redacted_summary: summary.to_string(),
    }
}

fn unresolved(code: &str, kind: UnresolvedIntentKindV1) -> UnresolvedIntentItemV1 {
    UnresolvedIntentItemV1 {
        kind,
        code: code.to_string(),
        redacted_detail: code.replace('_', " "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authoring_intent::{
        PROGRAM_INTENT_DRAFT_V1_SCHEMA, ProgramIntentOrigin, ReadinessIntentV1,
    };

    fn draft() -> ProgramIntentDraftV1 {
        ProgramIntentDraftV1 {
            schema: PROGRAM_INTENT_DRAFT_V1_SCHEMA.to_string(),
            origin: ProgramIntentOrigin::ManualSetup,
            toolchains: Vec::new(),
            build_steps: Vec::new(),
            launch: ProgramCommandDraftV1::Argv {
                argv: vec!["false".to_string()],
                cwd: WorkspacePathV1::root(),
                requested_environment: Vec::new(),
                required_tools: Vec::new(),
            },
            readiness: ReadinessIntentV1::Tcp {
                port: 8000,
                timeout_seconds: 60,
            },
            build_output_roots: Vec::new(),
            bindings: Vec::new(),
            unresolved: Vec::new(),
        }
    }

    fn event(sequence: u64, argv: &[&str]) -> SetupEventV1 {
        SetupEventV1 {
            sequence,
            command_argv: argv.iter().map(|part| (*part).to_string()).collect(),
            cwd: WorkspacePathV1::root(),
            exit_code: 0,
            started_at: "2026-07-28T00:00:00Z".to_string(),
            finished_at: "2026-07-28T00:00:01Z".to_string(),
            requested_environment_names: Vec::new(),
            network_artifacts: Vec::new(),
            filesystem_changes: Vec::new(),
            resolved_tools: Vec::new(),
        }
    }

    #[test]
    fn typed_journal_generates_build_and_launch_steps() {
        let journal = SetupJournalV1 {
            schema: SETUP_JOURNAL_V1_SCHEMA.to_string(),
            authoring_session_id: "as_1".to_string(),
            events: vec![
                event(1, &["pnpm", "install"]),
                event(2, &["pnpm", "build"]),
                event(3, &["python", "-m", "uvicorn", "app.main:app"]),
            ],
        };
        let analysis = apply_setup_journal(draft(), &journal).expect("analysis");
        assert_eq!(analysis.updated_draft.build_steps.len(), 2);
        assert_eq!(
            analysis.updated_draft.launch,
            ProgramCommandDraftV1::Argv {
                argv: vec![
                    "python".to_string(),
                    "-m".to_string(),
                    "uvicorn".to_string(),
                    "app.main:app".to_string()
                ],
                cwd: WorkspacePathV1::root(),
                requested_environment: Vec::new(),
                required_tools: Vec::new(),
            }
        );
        assert!(!analysis.publish_blocked);
    }

    #[test]
    fn host_binary_is_unreproducible() {
        let mut host = event(1, &["custom-host-tool"]);
        host.resolved_tools.push(ResolvedToolIdentityV1 {
            name: "custom-host-tool".to_string(),
            version: "1".to_string(),
            artifact_digest: format!("blake3:{}", "a".repeat(64)),
            provided_by_builder: false,
        });
        let analysis = apply_setup_journal(
            draft(),
            &SetupJournalV1 {
                schema: SETUP_JOURNAL_V1_SCHEMA.to_string(),
                authoring_session_id: "as_1".to_string(),
                events: vec![host],
            },
        )
        .expect("analysis");
        assert!(analysis.publish_blocked);
        assert_eq!(
            analysis.proposals[0].classification,
            SetupActionClassV1::UnreproducibleAction
        );
    }

    #[test]
    fn sensitive_file_becomes_binding_and_blocks_review() {
        let mut write = event(1, &["touch", ".env"]);
        write.filesystem_changes.push(FilesystemChangeV1 {
            path: WorkspacePathV1::parse(".env").expect("path"),
            kind: FilesystemChangeKindV1::Created,
        });
        let analysis = apply_setup_journal(
            draft(),
            &SetupJournalV1 {
                schema: SETUP_JOURNAL_V1_SCHEMA.to_string(),
                authoring_session_id: "as_1".to_string(),
                events: vec![write],
            },
        )
        .expect("analysis");
        assert!(analysis.publish_blocked);
        assert_eq!(
            analysis.updated_draft.bindings[0].kind,
            BindingKindV1::Secret
        );
    }

    #[test]
    fn secret_value_in_argv_is_rejected_before_analysis() {
        let journal = SetupJournalV1 {
            schema: SETUP_JOURNAL_V1_SCHEMA.to_string(),
            authoring_session_id: "as_1".to_string(),
            events: vec![event(1, &["curl", "--token=actual-secret"])],
        };
        assert!(matches!(
            apply_setup_journal(draft(), &journal),
            Err(SetupJournalError::PossibleSecretValue { sequence: 1 })
        ));
    }
}
