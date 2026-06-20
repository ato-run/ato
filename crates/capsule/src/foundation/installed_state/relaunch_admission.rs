//! Relaunch admission: evaluate an installed app's launch conditions from the
//! Installed-State DB ledger before relaunch (#508).
//!
//! ## Source of truth
//!
//! For installed-app relaunch, the launch conditions are read from the
//! `launch_condition_claims` ledger — **not** re-discovered from scattered
//! manifest / lockfile files. The DB is the device/provider-local SOT (#527);
//! this module turns the recorded claims into a typed pass / warn / block
//! decision.
//!
//! ## What blocks vs. warns
//!
//! A *required* condition blocks relaunch when it is `UserGrantRequired`,
//! `Missing`, `Unavailable`, `ProviderRequired`, or `Stale` — these are
//! knowable-before-launch unmet prerequisites. A required `Unknown` condition
//! does **not** block: install-time `Unknown` (e.g. a port declaration, or a
//! host-required env whose presence isn't verified yet) is resolved at launch by
//! the responsible subsystem (e.g. #523's port admission). Non-required
//! conditions never block; they surface as warnings.
//!
//! An empty ledger is **not** treated as "no conditions": it yields a
//! `LedgerMissing` warning (legacy installs predating the ledger continue), and
//! an incomplete baseline (`extraction_status.complete == false`) yields a
//! `LedgerIncomplete` warning rather than a block — extractor coverage is still
//! growing.
//!
//! No secret values appear in any reason: a reason carries a condition *key*
//! (the requirement's name), never its value.

use serde_json::Value;

use super::launch_condition::{
    LEDGER_EXTRACTION_STATUS_KEY, LaunchConditionClaim, LaunchConditionKind, LaunchConditionStatus,
};

/// Input to the relaunch admission evaluator: the installed app identity and the
/// launch conditions read from the ledger for that revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelaunchAdmissionInput {
    pub install_profile_key: String,
    pub install_revision_id: Option<String>,
    pub provider_id: Option<String>,
    pub claims: Vec<LaunchConditionClaim>,
}

/// The relaunch admission decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelaunchAdmission {
    /// Relaunch may proceed; `warnings` are non-blocking observations.
    Admitted {
        warnings: Vec<RelaunchAdmissionReason>,
    },
    /// Relaunch is blocked by `reasons`; `warnings` are additional context.
    Blocked {
        reasons: Vec<RelaunchAdmissionReason>,
        warnings: Vec<RelaunchAdmissionReason>,
    },
}

impl RelaunchAdmission {
    pub fn is_admitted(&self) -> bool {
        matches!(self, RelaunchAdmission::Admitted { .. })
    }

    /// Blocking reasons (empty when admitted).
    pub fn reasons(&self) -> &[RelaunchAdmissionReason] {
        match self {
            RelaunchAdmission::Admitted { .. } => &[],
            RelaunchAdmission::Blocked { reasons, .. } => reasons,
        }
    }

    pub fn warnings(&self) -> &[RelaunchAdmissionReason] {
        match self {
            RelaunchAdmission::Admitted { warnings }
            | RelaunchAdmission::Blocked { warnings, .. } => warnings,
        }
    }
}

/// A typed reason for a relaunch warning or block. Carries condition *keys*
/// (requirement names) only — never a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelaunchAdmissionReason {
    /// No conditions recorded for this revision (e.g. an install predating the
    /// ledger). Treated as a warning + legacy continuation, not "no conditions".
    LedgerMissing,
    /// The ledger exists but extractor coverage is incomplete; these kinds have
    /// not been extracted yet.
    LedgerIncomplete {
        missing_extractors: Vec<String>,
    },
    UserGrantRequired {
        kind: LaunchConditionKind,
        condition_key: String,
    },
    Missing {
        kind: LaunchConditionKind,
        condition_key: String,
    },
    Unavailable {
        kind: LaunchConditionKind,
        condition_key: String,
    },
    ProviderRequired {
        condition_key: String,
    },
    Stale {
        kind: LaunchConditionKind,
        condition_key: String,
    },
    Unknown {
        kind: LaunchConditionKind,
        condition_key: String,
    },
}

impl RelaunchAdmissionReason {
    /// A short, value-free human description (safe for logs / error text).
    pub fn describe(&self) -> String {
        match self {
            RelaunchAdmissionReason::LedgerMissing => {
                "no launch condition ledger recorded for this revision".to_string()
            }
            RelaunchAdmissionReason::LedgerIncomplete { missing_extractors } => {
                format!(
                    "launch condition coverage is incomplete (not yet extracted: {})",
                    missing_extractors.join(", ")
                )
            }
            RelaunchAdmissionReason::UserGrantRequired {
                kind,
                condition_key,
            } => format!("{} {condition_key} requires user grant", kind.as_str()),
            RelaunchAdmissionReason::Missing {
                kind,
                condition_key,
            } => format!("{} {condition_key} is missing", kind.as_str()),
            RelaunchAdmissionReason::Unavailable {
                kind,
                condition_key,
            } => format!("{} {condition_key} is unavailable", kind.as_str()),
            RelaunchAdmissionReason::ProviderRequired { condition_key } => {
                format!("{condition_key} requires a provider not available for local relaunch")
            }
            RelaunchAdmissionReason::Stale {
                kind,
                condition_key,
            } => format!(
                "{} {condition_key} is stale and needs repair",
                kind.as_str()
            ),
            RelaunchAdmissionReason::Unknown {
                kind,
                condition_key,
            } => format!("{} {condition_key} is unresolved", kind.as_str()),
        }
    }
}

/// Evaluate relaunch admission from the ledger conditions. Pure (no I/O).
pub fn evaluate_relaunch_admission(input: RelaunchAdmissionInput) -> RelaunchAdmission {
    // Empty ledger → warn (LedgerMissing) and let the caller continue the legacy
    // path. An empty ledger is never "no conditions".
    if input.claims.is_empty() {
        return RelaunchAdmission::Admitted {
            warnings: vec![RelaunchAdmissionReason::LedgerMissing],
        };
    }

    let mut reasons = Vec::new();
    let mut warnings = Vec::new();

    for claim in &input.claims {
        // The baseline marker is metadata, not a launch requirement: extract its
        // incompleteness as a warning and skip the generic status handling.
        if claim.condition_key == LEDGER_EXTRACTION_STATUS_KEY {
            if let Some(incomplete) = ledger_incomplete_reason(claim) {
                warnings.push(incomplete);
            }
            continue;
        }

        let reason = match claim.status {
            LaunchConditionStatus::Satisfied => continue,
            LaunchConditionStatus::Unknown => RelaunchAdmissionReason::Unknown {
                kind: claim.kind,
                condition_key: claim.condition_key.clone(),
            },
            LaunchConditionStatus::UserGrantRequired => {
                RelaunchAdmissionReason::UserGrantRequired {
                    kind: claim.kind,
                    condition_key: claim.condition_key.clone(),
                }
            }
            LaunchConditionStatus::Missing => RelaunchAdmissionReason::Missing {
                kind: claim.kind,
                condition_key: claim.condition_key.clone(),
            },
            LaunchConditionStatus::Unavailable => RelaunchAdmissionReason::Unavailable {
                kind: claim.kind,
                condition_key: claim.condition_key.clone(),
            },
            LaunchConditionStatus::ProviderRequired => RelaunchAdmissionReason::ProviderRequired {
                condition_key: claim.condition_key.clone(),
            },
            LaunchConditionStatus::Stale => RelaunchAdmissionReason::Stale {
                kind: claim.kind,
                condition_key: claim.condition_key.clone(),
            },
        };

        // A required condition blocks — except `Unknown`, which is resolved at
        // launch by the responsible subsystem. Non-required conditions warn.
        let blocks = claim.required && !matches!(claim.status, LaunchConditionStatus::Unknown);
        if blocks {
            reasons.push(reason);
        } else {
            warnings.push(reason);
        }
    }

    if reasons.is_empty() {
        RelaunchAdmission::Admitted { warnings }
    } else {
        RelaunchAdmission::Blocked { reasons, warnings }
    }
}

/// If the baseline marker reports incomplete coverage, build the warning.
fn ledger_incomplete_reason(marker: &LaunchConditionClaim) -> Option<RelaunchAdmissionReason> {
    let detail: Value = serde_json::from_str(&marker.detail_json).ok()?;
    if detail.get("complete").and_then(Value::as_bool) != Some(false) {
        return None;
    }
    let missing_extractors = detail
        .get("missing_extractors")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Some(RelaunchAdmissionReason::LedgerIncomplete { missing_extractors })
}

#[cfg(test)]
mod tests {
    use super::super::launch_condition::{
        LaunchConditionSource, launch_condition_extraction_status,
    };
    use super::*;

    fn input(claims: Vec<LaunchConditionClaim>) -> RelaunchAdmissionInput {
        RelaunchAdmissionInput {
            install_profile_key: "ipk_app".to_string(),
            install_revision_id: Some("rev1".to_string()),
            provider_id: None,
            claims,
        }
    }

    fn claim(
        kind: LaunchConditionKind,
        condition_key: &str,
        status: LaunchConditionStatus,
        required: bool,
    ) -> LaunchConditionClaim {
        LaunchConditionClaim {
            install_profile_key: "ipk_app".to_string(),
            install_revision_id: Some("rev1".to_string()),
            provider_id: None,
            kind,
            condition_key: condition_key.to_string(),
            status,
            required,
            source: LaunchConditionSource::Manifest,
            detail_json: "{}".to_string(),
            redacted: true,
        }
    }

    #[test]
    fn relaunch_admission_empty_ledger_warns_ledger_missing() {
        let decision = evaluate_relaunch_admission(input(vec![]));
        assert!(decision.is_admitted());
        assert_eq!(
            decision.warnings(),
            &[RelaunchAdmissionReason::LedgerMissing]
        );
    }

    #[test]
    fn relaunch_admission_satisfied_conditions_admit() {
        let decision = evaluate_relaunch_admission(input(vec![
            claim(
                LaunchConditionKind::Storage,
                "requirements.disk",
                LaunchConditionStatus::Satisfied,
                true,
            ),
            claim(
                LaunchConditionKind::State,
                "data",
                LaunchConditionStatus::Satisfied,
                true,
            ),
        ]));
        assert!(decision.is_admitted());
        assert!(decision.warnings().is_empty());
    }

    #[test]
    fn relaunch_admission_unknown_conditions_warn_but_admit() {
        // A required port declaration is Unknown at install — must not block.
        let decision = evaluate_relaunch_admission(input(vec![claim(
            LaunchConditionKind::Port,
            "ato://app/ipk_app/main.tcp",
            LaunchConditionStatus::Unknown,
            true,
        )]));
        assert!(
            decision.is_admitted(),
            "Unknown must not block: {decision:?}"
        );
        assert_eq!(decision.warnings().len(), 1);
        assert!(matches!(
            decision.warnings()[0],
            RelaunchAdmissionReason::Unknown { .. }
        ));
    }

    #[test]
    fn relaunch_admission_user_grant_required_blocks() {
        let decision = evaluate_relaunch_admission(input(vec![claim(
            LaunchConditionKind::Secret,
            "OPENAI_API_KEY",
            LaunchConditionStatus::UserGrantRequired,
            true,
        )]));
        assert!(!decision.is_admitted());
        assert!(matches!(
            decision.reasons()[0],
            RelaunchAdmissionReason::UserGrantRequired { .. }
        ));
        // The reason text carries the key (name), never a value.
        assert!(decision.reasons()[0].describe().contains("OPENAI_API_KEY"));
    }

    #[test]
    fn relaunch_admission_missing_required_condition_blocks() {
        let decision = evaluate_relaunch_admission(input(vec![claim(
            LaunchConditionKind::State,
            "data",
            LaunchConditionStatus::Missing,
            true,
        )]));
        assert!(!decision.is_admitted());
        assert!(matches!(
            decision.reasons()[0],
            RelaunchAdmissionReason::Missing { .. }
        ));
    }

    #[test]
    fn relaunch_admission_unavailable_required_condition_blocks() {
        let decision = evaluate_relaunch_admission(input(vec![claim(
            LaunchConditionKind::Runtime,
            "deno",
            LaunchConditionStatus::Unavailable,
            true,
        )]));
        assert!(!decision.is_admitted());
        assert!(matches!(
            decision.reasons()[0],
            RelaunchAdmissionReason::Unavailable { .. }
        ));
    }

    #[test]
    fn relaunch_admission_provider_required_blocks_local_relaunch() {
        let decision = evaluate_relaunch_admission(input(vec![claim(
            LaunchConditionKind::ProviderCapability,
            "gpu.nvidia.cuda",
            LaunchConditionStatus::ProviderRequired,
            true,
        )]));
        assert!(!decision.is_admitted());
        assert!(matches!(
            decision.reasons()[0],
            RelaunchAdmissionReason::ProviderRequired { .. }
        ));
    }

    #[test]
    fn relaunch_admission_non_required_missing_warns_only() {
        let decision = evaluate_relaunch_admission(input(vec![claim(
            LaunchConditionKind::Env,
            "OPTIONAL_FLAG",
            LaunchConditionStatus::Missing,
            false,
        )]));
        assert!(decision.is_admitted(), "non-required must not block");
        assert!(matches!(
            decision.warnings()[0],
            RelaunchAdmissionReason::Missing { .. }
        ));
    }

    #[test]
    fn relaunch_admission_incomplete_baseline_warns_missing_extractors() {
        let marker = launch_condition_extraction_status(
            "ipk_app",
            Some("rev1"),
            &[LaunchConditionKind::Storage],
        );
        let decision = evaluate_relaunch_admission(input(vec![
            marker,
            claim(
                LaunchConditionKind::Storage,
                "requirements.disk",
                LaunchConditionStatus::Satisfied,
                true,
            ),
        ]));
        assert!(decision.is_admitted());
        let incomplete = decision
            .warnings()
            .iter()
            .find_map(|w| match w {
                RelaunchAdmissionReason::LedgerIncomplete { missing_extractors } => {
                    Some(missing_extractors)
                }
                _ => None,
            })
            .expect("incomplete baseline must warn");
        assert!(incomplete.contains(&"secret".to_string()));
        // The marker itself is not treated as a launch requirement.
        assert!(
            decision
                .warnings()
                .iter()
                .all(|w| !matches!(w, RelaunchAdmissionReason::Unknown { .. })),
            "the extraction-status marker must not surface as an Unknown condition"
        );
    }

    #[test]
    fn relaunch_admission_collects_multiple_block_reasons() {
        let decision = evaluate_relaunch_admission(input(vec![
            claim(
                LaunchConditionKind::Secret,
                "OPENAI_API_KEY",
                LaunchConditionStatus::UserGrantRequired,
                true,
            ),
            claim(
                LaunchConditionKind::State,
                "data",
                LaunchConditionStatus::UserGrantRequired,
                true,
            ),
        ]));
        assert_eq!(decision.reasons().len(), 2);
    }
}
