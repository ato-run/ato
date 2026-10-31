//! One bounded requester tick shared by CLI and hosted exploration drivers.
//!
//! The coordinator remains authoritative for the frozen search and its
//! effects. A tick never treats the local deadline as a coordinator result,
//! and never dispatches a proposal after UNKNOWN, terminal stop, or expiry.
use anyhow::Result;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::Settlement;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverStep {
    Pending,
    NeedsInput,
    Settled(Settlement),
    Deadline,
}

/// Versioned hosted identity, distinct from the legacy CLI's absolute-journal
/// binding. This identifies a budget; it does not reserve or authorize a call.
/// A hosted driver must also use the coordinator's durable, fenced ledger.
pub fn hosted_budget_binding(owner: &str, search: &str) -> Result<String> {
    anyhow::ensure!(
        !owner.is_empty()
            && owner.len() <= 256
            && (8..=128).contains(&search.len())
            && search
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-'),
        "invalid hosted budget identity"
    );
    let bytes = serde_jcs::to_vec(&("ato.hosted-provider-budget-binding/1", owner, search))?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

/// Consume one observed coordinator snapshot. Callers provide the existing
/// decision/proposal transports, which retain their own claim and budget
/// fences; this helper does not create an alternate executor or journal.
pub fn drive_step(
    status: &Value,
    mut expired: impl FnMut() -> Result<bool>,
    decision: impl FnOnce() -> Result<()>,
    proposal: impl FnOnce() -> Result<()>,
) -> Result<DriverStep> {
    let settlement = Settlement::of(status)?;
    if settlement != Settlement::Running {
        return Ok(DriverStep::Settled(settlement));
    }
    if expired()? {
        return Ok(DriverStep::Deadline);
    }
    if status["pause_reason"] == "needs_input" {
        return Ok(DriverStep::NeedsInput);
    }
    decision()?;
    // A decision provider may have consumed the remaining deadline. Never
    // start a new proposal using a deadline check made before that call.
    if expired()? {
        return Ok(DriverStep::Deadline);
    }
    proposal()?;
    Ok(DriverStep::Pending)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;

    #[test]
    fn calls_decision_then_proposal_only_for_active_search() {
        let calls = RefCell::new(Vec::new());
        let result = drive_step(
            &json!({"status":"running"}),
            || Ok(false),
            || {
                calls.borrow_mut().push("decision");
                Ok(())
            },
            || {
                calls.borrow_mut().push("proposal");
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(result, DriverStep::Pending);
        assert_eq!(*calls.borrow(), ["decision", "proposal"]);
    }

    #[test]
    fn never_drives_paused_expired_unknown_or_settled_searches() {
        let cases = [
            (
                json!({"status":"running","pause_reason":"needs_input"}),
                false,
                DriverStep::NeedsInput,
            ),
            (json!({"status":"running"}), true, DriverStep::Deadline),
            (
                json!({"status":"unknown","unknown_attempts":[]}),
                false,
                DriverStep::Settled(Settlement::EffectUnknown(vec![])),
            ),
            (
                json!({"status":"stopped"}),
                false,
                DriverStep::Settled(Settlement::Stopped),
            ),
            (
                json!({"status":"satisfied"}),
                true,
                DriverStep::Settled(Settlement::Satisfied),
            ),
        ];
        for (status, expired, expected) in cases {
            let actual = drive_step(
                &status,
                || Ok(expired),
                || panic!("decision must not run"),
                || panic!("proposal must not run"),
            )
            .unwrap();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn invalid_status_and_decision_errors_do_not_issue_proposals() {
        assert!(
            drive_step(
                &json!({"status":"future"}),
                || Ok(false),
                || panic!("decision must not run"),
                || panic!("proposal must not run")
            )
            .is_err()
        );
        assert!(
            drive_step(
                &json!({"status":"running"}),
                || Ok(false),
                || anyhow::bail!("decision failed"),
                || panic!("proposal must not run")
            )
            .is_err()
        );
    }

    #[test]
    fn deadline_crossed_by_decision_never_starts_a_proposal() {
        let mut checks = 0;
        let result = drive_step(
            &json!({"status":"running"}),
            || {
                checks += 1;
                Ok(checks > 1)
            },
            || Ok(()),
            || panic!("proposal must not run"),
        )
        .unwrap();
        assert_eq!(result, DriverStep::Deadline);
    }

    #[test]
    fn hosted_binding_is_stable_and_separates_owners_searches_and_legacy() {
        let binding = hosted_budget_binding("owner-a", "search_123").unwrap();
        assert_eq!(
            binding,
            hosted_budget_binding("owner-a", "search_123").unwrap()
        );
        assert_ne!(
            binding,
            hosted_budget_binding("owner-b", "search_123").unwrap()
        );
        assert_ne!(
            binding,
            hosted_budget_binding("owner-a", "search_456").unwrap()
        );
        let legacy = format!(
            "sha256:{:x}",
            Sha256::digest(serde_jcs::to_vec(&("search_123", "/example/journal")).unwrap())
        );
        assert_ne!(binding, legacy);
        assert!(hosted_budget_binding("", "search_123").is_err());
        assert!(hosted_budget_binding("owner-a", "../escape").is_err());
    }
}
