//! Apply `capsule://` launch-condition query inputs to ledger claims, in memory,
//! before relaunch resolution (#508).
//!
//! A query input is **not** a proof. `secret.X=grant:<id>` /
//! `state.K=binding:<id>` merely *select* an existing logical grant / binding to
//! try — they set the matching claim's `grant_ref` / `binding_ref` in an
//! **in-memory** copy of the claims. Whether that grant/binding actually exists
//! is still decided by the resolver against the DB existence registry (#534): an
//! absent grant/binding leaves the claim `UserGrantRequired` and blocks. Nothing
//! here writes the `secret_grant_refs` / `state_binding_refs` registries, and no
//! raw value or host path is ever stored (the parser already rejected those, and
//! the merged detail is re-validated for redaction).
//!
//! `secret.*=grant:<id>`, `state.*=binding:<id>`, and (since #549) a sensitive
//! `env.*=grant:<id>` — the grant/binding-bearing `UserGrantRequired` kinds — are
//! overlaid: each sets the matching claim's `grant_ref` / `binding_ref` so the
//! resolver can confirm it.
//!
//! Out of scope this slice (all ignored here): `port` inputs (the launch-time
//! `PortClaim` admission, #523, owns the actual port), non-sensitive `env`
//! literals (runtime env injection is a follow-up), `env.*=required` (no grant to
//! overlay), and `env.*=prompt` (interactive env-grant creation is a follow-up).

use serde_json::Value;

use crate::error::{CapsuleError, Result};

use super::launch_condition::{
    LaunchConditionClaim, LaunchConditionKind, validate_redacted_detail_json,
};
use super::launch_input::{
    LaunchConditionInput, LaunchConditionInputKind, LaunchConditionInputValue,
};

fn apply_err(msg: impl Into<String>) -> CapsuleError {
    CapsuleError::Runtime(msg.into())
}

/// Overlay parsed query inputs onto a copy of the ledger claims.
///
/// Matching is by condition kind + condition key. The ledger may key a condition
/// either bare (`OPENAI_API_KEY`) or namespaced (`secret.OPENAI_API_KEY`); both
/// match the query key `secret.OPENAI_API_KEY`.
///
/// - `secret.<name>=grant:<id>` → set the matching claim's `grant_ref` to `<id>`
///   (status unchanged — the resolver confirms existence).
/// - `env.<name>=grant:<id>` → set the matching Env claim's `grant_ref` (#549),
///   same mechanism as secret (status unchanged — the resolver confirms it).
/// - `state.<key>=binding:<id>` → set the matching claim's `binding_ref`.
/// - `secret`/`state`/`env`-grant input with **no** matching claim → error (the
///   user referenced a condition this app does not declare).
/// - `port.*`, env *literals*, `env.*=required`, `env.*=prompt`, and
///   `use-existing` are ignored this slice (see the module docs).
pub fn apply_capsule_launch_inputs_to_claims(
    claims: &[LaunchConditionClaim],
    inputs: &[LaunchConditionInput],
) -> Result<Vec<LaunchConditionClaim>> {
    let mut claims = claims.to_vec();
    for input in inputs {
        let Some((kind, namespace, detail_key)) = actionable(input) else {
            // Ignored input (port / env literal / required / use-existing).
            continue;
        };
        let Some(locator) = locator(input) else {
            // A recognized-but-no-locator input (e.g. `required`): require that
            // the condition exists, but make no change.
            if !claims
                .iter()
                .any(|c| c.kind == kind && matches_key(&c.condition_key, namespace, &input.key))
            {
                return Err(apply_err(format!(
                    "launch input references unknown condition '{namespace}.{}'",
                    input.key
                )));
            }
            continue;
        };

        let claim = claims
            .iter_mut()
            .find(|c| c.kind == kind && matches_key(&c.condition_key, namespace, &input.key))
            .ok_or_else(|| {
                apply_err(format!(
                    "launch input references unknown condition '{namespace}.{}'",
                    input.key
                ))
            })?;
        set_detail_ref(claim, detail_key, &locator)?;
    }
    Ok(claims)
}

/// For an input that overlays a claim, return `(ledger kind, query namespace,
/// detail key)`. `None` for inputs ignored by this slice.
fn actionable(
    input: &LaunchConditionInput,
) -> Option<(LaunchConditionKind, &'static str, &'static str)> {
    match input.kind {
        LaunchConditionInputKind::Secret => {
            Some((LaunchConditionKind::Secret, "secret", "grant_ref"))
        }
        LaunchConditionInputKind::State => {
            Some((LaunchConditionKind::State, "state", "binding_ref"))
        }
        // #549: a sensitive `env.<name>=grant:<id>` overlays a grant_ref onto the
        // matching Env claim, so the resolver can satisfy it by grant (mirroring
        // Secret). Only the *grant* form is actionable: an env literal or
        // `env.K=required` carries no grant to inject and stays ignored (literal
        // runtime injection and `env.K=prompt` creation are follow-ups), so this
        // slice does not start enforcing existence for plain env literals.
        LaunchConditionInputKind::Env
            if matches!(input.value, LaunchConditionInputValue::Grant(_)) =>
        {
            Some((LaunchConditionKind::Env, "env", "grant_ref"))
        }
        // Port / env literal / env required / others are ignored this slice.
        _ => None,
    }
}

/// The locator id carried by a Grant/Binding input, if any.
fn locator(input: &LaunchConditionInput) -> Option<String> {
    match &input.value {
        LaunchConditionInputValue::Grant(id) | LaunchConditionInputValue::Binding(id) => {
            Some(id.clone())
        }
        _ => None,
    }
}

/// Does `claim_key` match the query key `<namespace>.<input_key>`? Accepts both
/// the bare (`input_key`) and namespaced (`namespace.input_key`) ledger forms.
fn matches_key(claim_key: &str, namespace: &str, input_key: &str) -> bool {
    claim_key == input_key || claim_key == format!("{namespace}.{input_key}")
}

/// Merge a single `key = <logical id>` into the claim's redacted `detail_json`,
/// preserving the other detail fields and re-validating redaction.
fn set_detail_ref(claim: &mut LaunchConditionClaim, key: &str, id: &str) -> Result<()> {
    let mut detail = match serde_json::from_str::<Value>(&claim.detail_json) {
        Ok(Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    detail.insert(key.to_string(), Value::String(id.to_string()));
    let detail_json = Value::Object(detail).to_string();
    // Defence in depth: the locator is a validated logical id, but re-check that
    // the merged detail carries no raw value before we keep it.
    validate_redacted_detail_json(claim.kind, &detail_json)?;
    claim.detail_json = detail_json;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::launch_condition::{LaunchConditionSource, LaunchConditionStatus};
    use super::*;

    fn claim(
        kind: LaunchConditionKind,
        condition_key: &str,
        detail_json: &str,
    ) -> LaunchConditionClaim {
        LaunchConditionClaim {
            install_profile_key: "ipk".to_string(),
            install_revision_id: Some("rev1".to_string()),
            provider_id: None,
            kind,
            condition_key: condition_key.to_string(),
            status: LaunchConditionStatus::UserGrantRequired,
            required: true,
            source: LaunchConditionSource::Manifest,
            detail_json: detail_json.to_string(),
            redacted: true,
        }
    }

    fn input(
        kind: LaunchConditionInputKind,
        key: &str,
        value: LaunchConditionInputValue,
    ) -> LaunchConditionInput {
        LaunchConditionInput {
            kind,
            key: key.to_string(),
            value,
        }
    }

    fn find<'a>(claims: &'a [LaunchConditionClaim], key: &str) -> &'a LaunchConditionClaim {
        claims.iter().find(|c| c.condition_key == key).unwrap()
    }

    #[test]
    fn secret_grant_input_sets_grant_ref_without_changing_status() {
        let claims = vec![claim(
            LaunchConditionKind::Secret,
            "OPENAI_API_KEY",
            r#"{"projection":"env","grant_ref":null}"#,
        )];
        let out = apply_capsule_launch_inputs_to_claims(
            &claims,
            &[input(
                LaunchConditionInputKind::Secret,
                "OPENAI_API_KEY",
                LaunchConditionInputValue::Grant("openai-default".to_string()),
            )],
        )
        .unwrap();
        let c = find(&out, "OPENAI_API_KEY");
        assert!(c.detail_json.contains("\"grant_ref\":\"openai-default\""));
        // The overlay is not a proof: status is still UserGrantRequired.
        assert_eq!(c.status, LaunchConditionStatus::UserGrantRequired);
    }

    #[test]
    fn secret_grant_input_matches_namespaced_ledger_key() {
        let claims = vec![claim(
            LaunchConditionKind::Secret,
            "secret.OPENAI_API_KEY",
            "{}",
        )];
        let out = apply_capsule_launch_inputs_to_claims(
            &claims,
            &[input(
                LaunchConditionInputKind::Secret,
                "OPENAI_API_KEY",
                LaunchConditionInputValue::Grant("g1".to_string()),
            )],
        )
        .unwrap();
        assert!(
            find(&out, "secret.OPENAI_API_KEY")
                .detail_json
                .contains("\"grant_ref\":\"g1\"")
        );
    }

    #[test]
    fn state_binding_input_sets_binding_ref() {
        let claims = vec![claim(
            LaunchConditionKind::State,
            "data",
            r#"{"durability":"persistent"}"#,
        )];
        let out = apply_capsule_launch_inputs_to_claims(
            &claims,
            &[input(
                LaunchConditionInputKind::State,
                "data",
                LaunchConditionInputValue::Binding("user-data".to_string()),
            )],
        )
        .unwrap();
        let c = find(&out, "data");
        assert!(c.detail_json.contains("\"binding_ref\":\"user-data\""));
        assert!(c.detail_json.contains("\"durability\":\"persistent\""));
    }

    #[test]
    fn env_grant_input_sets_grant_ref_without_changing_status() {
        // #549: a sensitive `env.K=grant:<id>` overlays the grant_ref onto the Env
        // claim (status unchanged — the resolver confirms grant existence).
        let claims = vec![claim(
            LaunchConditionKind::Env,
            "MY_TOKEN",
            r#"{"source":"manifest.required_env"}"#,
        )];
        let out = apply_capsule_launch_inputs_to_claims(
            &claims,
            &[input(
                LaunchConditionInputKind::Env,
                "MY_TOKEN",
                LaunchConditionInputValue::Grant("tok-1".to_string()),
            )],
        )
        .unwrap();
        let c = find(&out, "MY_TOKEN");
        assert!(c.detail_json.contains("\"grant_ref\":\"tok-1\""));
        assert_eq!(c.status, LaunchConditionStatus::UserGrantRequired);
    }

    #[test]
    fn env_grant_input_matches_namespaced_env_ledger_key() {
        let claims = vec![claim(LaunchConditionKind::Env, "env.MY_TOKEN", "{}")];
        let out = apply_capsule_launch_inputs_to_claims(
            &claims,
            &[input(
                LaunchConditionInputKind::Env,
                "MY_TOKEN",
                LaunchConditionInputValue::Grant("tok-1".to_string()),
            )],
        )
        .unwrap();
        assert!(
            find(&out, "env.MY_TOKEN")
                .detail_json
                .contains("\"grant_ref\":\"tok-1\"")
        );
    }

    #[test]
    fn env_grant_input_for_unknown_condition_errors() {
        let claims = vec![claim(LaunchConditionKind::Env, "OTHER", "{}")];
        let err = apply_capsule_launch_inputs_to_claims(
            &claims,
            &[input(
                LaunchConditionInputKind::Env,
                "MY_TOKEN",
                LaunchConditionInputValue::Grant("tok-1".to_string()),
            )],
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown condition 'env.MY_TOKEN'"));
    }

    #[test]
    fn env_literal_input_is_still_ignored() {
        // A non-sensitive env literal carries no grant; it stays ignored (no
        // overlay, and crucially no unknown-condition enforcement).
        let claims = vec![claim(LaunchConditionKind::Env, "LOG_LEVEL", "{}")];
        let out = apply_capsule_launch_inputs_to_claims(
            &claims,
            &[input(
                LaunchConditionInputKind::Env,
                "NOT_DECLARED",
                LaunchConditionInputValue::Literal("debug".to_string()),
            )],
        )
        .unwrap();
        assert_eq!(
            out, claims,
            "env literal makes no change and does not error"
        );
    }

    #[test]
    fn unknown_condition_errors() {
        let claims = vec![claim(LaunchConditionKind::Secret, "OTHER", "{}")];
        let err = apply_capsule_launch_inputs_to_claims(
            &claims,
            &[input(
                LaunchConditionInputKind::Secret,
                "OPENAI_API_KEY",
                LaunchConditionInputValue::Grant("g1".to_string()),
            )],
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown condition"));
    }

    #[test]
    fn port_and_env_literal_inputs_are_ignored() {
        let claims = vec![
            claim(LaunchConditionKind::Port, "main.tcp", "{}"),
            claim(LaunchConditionKind::Env, "LOG_LEVEL", "{}"),
        ];
        let out = apply_capsule_launch_inputs_to_claims(
            &claims,
            &[
                input(
                    LaunchConditionInputKind::Port,
                    "main",
                    LaunchConditionInputValue::Literal("3001".to_string()),
                ),
                input(
                    LaunchConditionInputKind::Env,
                    "LOG_LEVEL",
                    LaunchConditionInputValue::Literal("debug".to_string()),
                ),
            ],
        )
        .unwrap();
        // No claim mutated; no error even though a port/env literal was supplied.
        assert_eq!(out, claims);
    }

    #[test]
    fn required_input_recognizes_existing_condition_without_change() {
        let claims = vec![claim(LaunchConditionKind::Secret, "OPENAI_API_KEY", "{}")];
        let out = apply_capsule_launch_inputs_to_claims(
            &claims,
            &[input(
                LaunchConditionInputKind::Secret,
                "OPENAI_API_KEY",
                LaunchConditionInputValue::Required,
            )],
        )
        .unwrap();
        assert_eq!(out, claims, "`required` makes no change");
    }

    #[test]
    fn prompt_input_is_not_proof_and_makes_no_change() {
        // Until the interactive creation flow rewrites it to `grant:<id>`, a
        // `=prompt` input overlays nothing: the condition stays UserGrantRequired.
        let claims = vec![claim(LaunchConditionKind::Secret, "OPENAI_API_KEY", "{}")];
        let out = apply_capsule_launch_inputs_to_claims(
            &claims,
            &[input(
                LaunchConditionInputKind::Secret,
                "OPENAI_API_KEY",
                LaunchConditionInputValue::Prompt,
            )],
        )
        .unwrap();
        assert_eq!(out, claims, "`prompt` is a request, not a proof");
    }

    #[test]
    fn prompt_input_for_unknown_condition_errors() {
        let claims = vec![claim(LaunchConditionKind::State, "other", "{}")];
        assert!(
            apply_capsule_launch_inputs_to_claims(
                &claims,
                &[input(
                    LaunchConditionInputKind::State,
                    "data",
                    LaunchConditionInputValue::Prompt,
                )],
            )
            .is_err()
        );
    }

    #[test]
    fn required_input_for_unknown_condition_errors() {
        let claims = vec![claim(LaunchConditionKind::Secret, "OTHER", "{}")];
        assert!(
            apply_capsule_launch_inputs_to_claims(
                &claims,
                &[input(
                    LaunchConditionInputKind::Secret,
                    "OPENAI_API_KEY",
                    LaunchConditionInputValue::Required,
                )],
            )
            .is_err()
        );
    }
}
