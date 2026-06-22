//! Plan which launch conditions need an interactive prompt (#508).
//!
//! `secret.<name>=prompt` / `state.<key>=prompt` are *requests to be prompted*,
//! not proofs. This module decides — purely from the installed-state ledger
//! claims and the parsed `capsule://` query inputs — which conditions actually
//! need a prompt. It performs **no I/O**: it never reads a secret store, never
//! writes the `secret_grant_refs` / `state_binding_refs` registries, and never
//! prompts the user.
//!
//! The interactive creation flow that consumes this plan is a later slice:
//! - `secret.<name>=prompt` → read a value, write it to the secure secret store,
//!   record a `secret_grant_ref`, and rewrite the input to `grant:<id>`;
//! - `state.<key>=prompt` → requires a manifest-free state-binding *target* store
//!   that does not exist yet, so it will return a typed "not implemented" error
//!   until that store is designed.
//!
//! Either way, a prompt only becomes a proof after the user acts and the
//! underlying secure / local-private write succeeds — never from the URL alone.

use crate::error::{CapsuleError, Result};

use super::launch_condition::{LaunchConditionClaim, LaunchConditionKind, LaunchConditionStatus};
use super::launch_input::{
    LaunchConditionInput, LaunchConditionInputKind, LaunchConditionInputValue,
};

/// A single launch condition that requested an interactive prompt and is not yet
/// satisfied by the ledger. Carries only identity — never a secret value or host
/// path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchConditionPromptRequest {
    /// The input kind — `Secret` or `State` for this slice.
    pub kind: LaunchConditionInputKind,
    /// The matching ledger claim's condition key, as stored (bare `OPENAI_API_KEY`
    /// or namespaced `secret.OPENAI_API_KEY`).
    pub condition_key: String,
    /// The per-kind key carried by the query input (e.g. `OPENAI_API_KEY`, `data`).
    pub input_key: String,
}

fn plan_err(msg: impl Into<String>) -> CapsuleError {
    CapsuleError::Runtime(msg.into())
}

/// Plan the interactive prompts implied by `=prompt` query inputs.
///
/// For each input whose value is [`LaunchConditionInputValue::Prompt`]:
/// - it must be a `secret` / `state` condition (the parser only ever produces
///   `Prompt` for those); any other kind is a typed error (defensive);
/// - it must match a ledger claim — bare (`K`) or namespaced (`secret.K` /
///   `state.K`); an unmatched prompt is a typed "unknown condition" error;
/// - a claim already [`Satisfied`](LaunchConditionStatus::Satisfied) needs no
///   prompt and is skipped;
/// - otherwise (e.g. `UserGrantRequired` / `Unknown`) a prompt is required and a
///   [`LaunchConditionPromptRequest`] is emitted.
///
/// Non-`prompt` inputs (`grant:` / `binding:` / `required` / `use-existing` /
/// literal) are ignored here — they are handled by the overlay / resolver. This
/// function is pure: it writes nothing and reads no store.
pub fn plan_launch_condition_prompts(
    claims: &[LaunchConditionClaim],
    inputs: &[LaunchConditionInput],
) -> Result<Vec<LaunchConditionPromptRequest>> {
    let mut plan = Vec::new();
    for input in inputs {
        if input.value != LaunchConditionInputValue::Prompt {
            // Only `=prompt` inputs are planned here; everything else is the
            // overlay/resolver's concern.
            continue;
        }
        let (ledger_kind, namespace) = match input.kind {
            LaunchConditionInputKind::Secret => (LaunchConditionKind::Secret, "secret"),
            LaunchConditionInputKind::State => (LaunchConditionKind::State, "state"),
            other => {
                // The parser never emits `Prompt` for these; fail closed if it
                // somehow reaches here rather than silently dropping a request.
                return Err(plan_err(format!(
                    "prompt is only supported for secret/state launch conditions, not {other:?}"
                )));
            }
        };
        let claim = claims
            .iter()
            .find(|c| c.kind == ledger_kind && matches_key(&c.condition_key, namespace, &input.key))
            .ok_or_else(|| {
                plan_err(format!(
                    "launch input references unknown condition '{namespace}.{}'",
                    input.key
                ))
            })?;
        if claim.status == LaunchConditionStatus::Satisfied {
            // Already satisfied by the ledger — no prompt needed.
            continue;
        }
        plan.push(LaunchConditionPromptRequest {
            kind: input.kind,
            condition_key: claim.condition_key.clone(),
            input_key: input.key.clone(),
        });
    }
    Ok(plan)
}

/// Does `claim_key` match the query key `<namespace>.<input_key>`? Accepts both
/// the bare (`input_key`) and namespaced (`namespace.input_key`) ledger forms —
/// the same matching used by the input overlay.
fn matches_key(claim_key: &str, namespace: &str, input_key: &str) -> bool {
    claim_key == input_key || claim_key == format!("{namespace}.{input_key}")
}

#[cfg(test)]
mod tests {
    use super::super::launch_condition::LaunchConditionSource;
    use super::*;

    fn claim(
        kind: LaunchConditionKind,
        condition_key: &str,
        status: LaunchConditionStatus,
    ) -> LaunchConditionClaim {
        LaunchConditionClaim {
            install_profile_key: "ipk".to_string(),
            install_revision_id: Some("rev1".to_string()),
            provider_id: None,
            kind,
            condition_key: condition_key.to_string(),
            status,
            required: true,
            source: LaunchConditionSource::Manifest,
            detail_json: "{}".to_string(),
            redacted: true,
        }
    }

    fn prompt(kind: LaunchConditionInputKind, key: &str) -> LaunchConditionInput {
        LaunchConditionInput {
            kind,
            key: key.to_string(),
            value: LaunchConditionInputValue::Prompt,
        }
    }

    #[test]
    fn plan_prompts_finds_secret_prompt_for_unresolved_claim() {
        let claims = vec![claim(
            LaunchConditionKind::Secret,
            "OPENAI_API_KEY",
            LaunchConditionStatus::UserGrantRequired,
        )];
        let plan = plan_launch_condition_prompts(
            &claims,
            &[prompt(LaunchConditionInputKind::Secret, "OPENAI_API_KEY")],
        )
        .unwrap();
        assert_eq!(
            plan,
            vec![LaunchConditionPromptRequest {
                kind: LaunchConditionInputKind::Secret,
                condition_key: "OPENAI_API_KEY".to_string(),
                input_key: "OPENAI_API_KEY".to_string(),
            }]
        );
    }

    #[test]
    fn plan_prompts_finds_state_prompt_for_unresolved_claim() {
        let claims = vec![claim(
            LaunchConditionKind::State,
            "data",
            LaunchConditionStatus::UserGrantRequired,
        )];
        let plan = plan_launch_condition_prompts(
            &claims,
            &[prompt(LaunchConditionInputKind::State, "data")],
        )
        .unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].kind, LaunchConditionInputKind::State);
        assert_eq!(plan[0].condition_key, "data");
    }

    #[test]
    fn plan_prompts_matches_namespaced_ledger_key() {
        // The ledger may store the condition namespaced; a bare input still matches.
        let claims = vec![claim(
            LaunchConditionKind::Secret,
            "secret.OPENAI_API_KEY",
            LaunchConditionStatus::Unknown,
        )];
        let plan = plan_launch_condition_prompts(
            &claims,
            &[prompt(LaunchConditionInputKind::Secret, "OPENAI_API_KEY")],
        )
        .unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].condition_key, "secret.OPENAI_API_KEY");
    }

    #[test]
    fn plan_prompts_plans_unknown_status_secret() {
        // `Unknown` (non-blocking-but-surfaced) still gets a prompt — it is not
        // yet Satisfied.
        let claims = vec![claim(
            LaunchConditionKind::Secret,
            "OPENAI_API_KEY",
            LaunchConditionStatus::Unknown,
        )];
        let plan = plan_launch_condition_prompts(
            &claims,
            &[prompt(LaunchConditionInputKind::Secret, "OPENAI_API_KEY")],
        )
        .unwrap();
        assert_eq!(plan.len(), 1);
    }

    #[test]
    fn plan_prompts_skips_already_satisfied_secret() {
        let claims = vec![claim(
            LaunchConditionKind::Secret,
            "OPENAI_API_KEY",
            LaunchConditionStatus::Satisfied,
        )];
        let plan = plan_launch_condition_prompts(
            &claims,
            &[prompt(LaunchConditionInputKind::Secret, "OPENAI_API_KEY")],
        )
        .unwrap();
        assert!(plan.is_empty(), "a satisfied condition needs no prompt");
    }

    #[test]
    fn plan_prompts_errors_on_unknown_secret_condition() {
        let claims = vec![claim(
            LaunchConditionKind::Secret,
            "OTHER",
            LaunchConditionStatus::UserGrantRequired,
        )];
        let err = plan_launch_condition_prompts(
            &claims,
            &[prompt(LaunchConditionInputKind::Secret, "OPENAI_API_KEY")],
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown condition"));
    }

    #[test]
    fn plan_prompts_errors_on_unknown_state_condition() {
        let claims = vec![claim(
            LaunchConditionKind::State,
            "other",
            LaunchConditionStatus::UserGrantRequired,
        )];
        let err = plan_launch_condition_prompts(
            &claims,
            &[prompt(LaunchConditionInputKind::State, "data")],
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown condition"));
    }

    #[test]
    fn plan_prompts_ignores_non_prompt_inputs() {
        // grant:/binding:/required inputs are the overlay/resolver's job, not a
        // prompt — even when their claim is unresolved.
        let claims = vec![
            claim(
                LaunchConditionKind::Secret,
                "OPENAI_API_KEY",
                LaunchConditionStatus::UserGrantRequired,
            ),
            claim(
                LaunchConditionKind::State,
                "data",
                LaunchConditionStatus::UserGrantRequired,
            ),
        ];
        let inputs = vec![
            LaunchConditionInput {
                kind: LaunchConditionInputKind::Secret,
                key: "OPENAI_API_KEY".to_string(),
                value: LaunchConditionInputValue::Grant("g1".to_string()),
            },
            LaunchConditionInput {
                kind: LaunchConditionInputKind::State,
                key: "data".to_string(),
                value: LaunchConditionInputValue::Binding("user-data".to_string()),
            },
            LaunchConditionInput {
                kind: LaunchConditionInputKind::Secret,
                key: "OPENAI_API_KEY".to_string(),
                value: LaunchConditionInputValue::Required,
            },
        ];
        assert!(
            plan_launch_condition_prompts(&claims, &inputs)
                .unwrap()
                .is_empty()
        );
    }
}
