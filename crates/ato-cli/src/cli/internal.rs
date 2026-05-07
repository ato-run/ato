//! Hidden plumbing surface for trusted shells (today: ato-desktop).
//!
//! Each subcommand here is `#[command(hide = true)]` because it is a
//! plumbing endpoint, not a user-facing command. Stability guarantees
//! are weaker than the public CLI: arguments may evolve in lockstep
//! with the calling shell.

use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum InternalCommands {
    /// Consent-store plumbing surface. Currently only carries the
    /// approve-execution-plan endpoint that the desktop's E302 modal
    /// (and the matching `approve_execution_plan_consent` MCP tool)
    /// calls after the user approves the plan summary.
    #[command(hide = true, about = "Consent-store plumbing for trusted shells")]
    Consent {
        #[command(subcommand)]
        command: ConsentInternalCommands,
    },
}

#[derive(Subcommand)]
pub(crate) enum ConsentInternalCommands {
    /// Append an ExecutionPlan consent record to
    /// `${ATO_HOME:-~/.ato}/consent/executionplan_v1.jsonl` using
    /// the same code path interactive prompts go through. Idempotent:
    /// if the matching record is already present, no new line is
    /// appended. The five identity fields must match exactly what
    /// shipped in the most recent `execution_plan_consent_required`
    /// envelope for the capsule.
    ///
    /// Owns: ATO_HOME resolution, parent-dir 0o700, file 0o600,
    /// JSONL append. The desktop must NOT write the consent file
    /// directly — call this command instead.
    #[command(
        hide = true,
        about = "Append an ExecutionPlan consent record (plumbing)"
    )]
    ApproveExecutionPlan {
        /// `plan.consent.key.scoped_id`
        #[arg(long)]
        scoped_id: String,
        /// `plan.consent.key.version`
        #[arg(long)]
        version: String,
        /// `plan.consent.key.target_label`
        #[arg(long)]
        target_label: String,
        /// `plan.consent.policy_segment_hash`
        #[arg(long)]
        policy_segment_hash: String,
        /// `plan.consent.provisioning_policy_hash`
        #[arg(long)]
        provisioning_policy_hash: String,
        /// Emit a single-line JSON envelope on stdout, parse-friendly
        /// for the desktop's CLI envelope reader. Mirrors the `--json`
        /// convention used by other plumbing commands.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
}
