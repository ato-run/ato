//! Provider spend-plan validation shared by local and hosted authorities.
//! These limits are operational; they are never Capsule or Contract identity.
use crate::exploration::ExplorationPolicy;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

pub const AUTHORIZED_USD_MICROS: u64 = 5_000_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetPlan {
    pub max_calls: u32,
    pub input_token_cap: u64,
    pub output_token_cap: u64,
    /// Explicit peak/cache-miss price snapshot, micros per million tokens.
    pub input_price: u64,
    pub output_price: u64,
    pub ceiling_usd_micros: u64,
}

impl BudgetPlan {
    pub fn cost(&self, input: u64, output: u64) -> Result<u64> {
        let input = (u128::from(input) * u128::from(self.input_price)).div_ceil(1_000_000);
        let output = (u128::from(output) * u128::from(self.output_price)).div_ceil(1_000_000);
        Ok(u64::try_from(
            input
                .checked_add(output)
                .ok_or_else(|| anyhow::anyhow!("cost overflow"))?,
        )?)
    }

    pub fn validate(&self) -> Result<u64> {
        ensure!(
            (1..=64).contains(&self.max_calls)
                && self.input_token_cap > 0
                && (1..=if self.output_price == 0 { 65536 } else { 2048 })
                    .contains(&self.output_token_cap)
                && self.input_price > 0
                && self.ceiling_usd_micros <= AUTHORIZED_USD_MICROS,
            "invalid spend plan"
        );
        let per_call = self.cost(self.input_token_cap, self.output_token_cap)?;
        ensure!(
            per_call
                .checked_mul(u64::from(self.max_calls))
                .is_some_and(|n| n <= self.ceiling_usd_micros),
            "spend ceiling exceeded"
        );
        Ok(per_call)
    }
}

/// Validate a candidate and optional decision provider together against the
/// already-frozen Search ceilings. This validates limits only: callers must
/// separately bind each plan to its validated provider configuration.
pub fn validate_combined(plans: &[BudgetPlan], exploration: &ExplorationPolicy) -> Result<()> {
    ensure!(
        (1..=2).contains(&plans.len()),
        "invalid provider plan count"
    );
    exploration
        .validate()
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let mut calls = 0_u64;
    let mut input = 0_u64;
    let mut output = 0_u64;
    let mut cost = 0_u64;
    for plan in plans {
        let count = u64::from(plan.max_calls);
        let reserved = plan.validate()?;
        calls = calls
            .checked_add(count)
            .ok_or_else(|| anyhow::anyhow!("calls overflow"))?;
        input = input
            .checked_add(
                plan.input_token_cap
                    .checked_mul(count)
                    .ok_or_else(|| anyhow::anyhow!("token overflow"))?,
            )
            .ok_or_else(|| anyhow::anyhow!("token overflow"))?;
        output = output
            .checked_add(
                plan.output_token_cap
                    .checked_mul(count)
                    .ok_or_else(|| anyhow::anyhow!("token overflow"))?,
            )
            .ok_or_else(|| anyhow::anyhow!("token overflow"))?;
        cost = cost
            .checked_add(
                reserved
                    .checked_mul(count)
                    .ok_or_else(|| anyhow::anyhow!("spend overflow"))?,
            )
            .ok_or_else(|| anyhow::anyhow!("spend overflow"))?;
    }
    ensure!(
        calls <= u64::from(exploration.max_provider_calls)
            && input <= exploration.max_provider_input_tokens
            && output <= exploration.max_provider_output_tokens
            && cost <= exploration.max_provider_cost_usd_micros,
        "combined provider spend plan exceeds frozen exploration budget"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn costs_round_up_and_enforce_the_full_reserved_plan() {
        let mut plan = BudgetPlan {
            max_calls: 6,
            input_token_cap: 262_144,
            output_token_cap: 2048,
            input_price: 300_000,
            output_price: 1_200_000,
            ceiling_usd_micros: 5_000_000,
        };
        assert_eq!(plan.validate().unwrap(), 81_102);
        plan.ceiling_usd_micros = 486_611;
        assert!(plan.validate().is_err());
        plan.ceiling_usd_micros = 5_000_000;
        plan.max_calls = 65;
        assert!(plan.validate().is_err());
    }

    #[test]
    fn combined_plans_cannot_exceed_any_frozen_search_ceiling() {
        let plan = BudgetPlan {
            max_calls: 2,
            input_token_cap: 100,
            output_token_cap: 20,
            input_price: 1,
            output_price: 1,
            ceiling_usd_micros: 4,
        };
        let policy: ExplorationPolicy = serde_json::from_value(serde_json::json!({
            "ceiling":{}, "max_provider_calls":4, "max_inspections":0,
            "max_provider_cost_usd_micros":8, "max_provider_input_tokens":400,
            "max_provider_output_tokens":80
        }))
        .unwrap();
        assert!(validate_combined(&[plan.clone(), plan.clone()], &policy).is_ok());
        let mut limited = policy.clone();
        limited.max_provider_calls -= 1;
        assert!(validate_combined(&[plan.clone(), plan.clone()], &limited).is_err());
        limited = policy.clone();
        limited.max_provider_input_tokens -= 1;
        assert!(validate_combined(&[plan.clone(), plan.clone()], &limited).is_err());
        limited = policy.clone();
        limited.max_provider_output_tokens -= 1;
        assert!(validate_combined(&[plan.clone(), plan.clone()], &limited).is_err());
        limited = policy.clone();
        limited.max_provider_cost_usd_micros -= 1;
        assert!(validate_combined(&[plan.clone(), plan], &limited).is_err());
        assert!(validate_combined(&[], &policy).is_err());
    }
}
