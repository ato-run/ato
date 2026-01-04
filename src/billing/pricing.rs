//! Pricing and Billing Calculation
//!
//! Converts usage data (from usage.rs) into billable amounts.
//!
//! Uses rust_decimal for precise monetary calculations to avoid
//! floating-point rounding errors.

use anyhow::Result;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

/// Pricing tiers for different subscription levels
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingTier {
    /// Tier name (free, starter, pro, enterprise)
    pub name: String,
    /// Base monthly fee (USD)
    pub base_fee: Decimal,
    /// Included quota (tokens per month)
    pub included_tokens: u64,
    /// Price per additional 1M tokens (USD)
    pub price_per_million_tokens: Decimal,
    /// Included GPU hours per month
    pub included_gpu_hours: Decimal,
    /// Price per GPU hour (USD)
    pub price_per_gpu_hour: Decimal,
}

impl PricingTier {
    /// Free tier - local only
    pub fn free() -> Self {
        Self {
            name: "free".to_string(),
            base_fee: dec!(0),
            included_tokens: 1_000_000, // 1M tokens/month
            price_per_million_tokens: dec!(0),
            included_gpu_hours: dec!(0),
            price_per_gpu_hour: dec!(0),
        }
    }

    /// Starter tier - $9/month
    pub fn starter() -> Self {
        Self {
            name: "starter".to_string(),
            base_fee: dec!(9),
            included_tokens: 10_000_000, // 10M tokens/month
            price_per_million_tokens: dec!(0.50),
            included_gpu_hours: dec!(5),
            price_per_gpu_hour: dec!(1.50),
        }
    }

    /// Pro tier - $29/month
    pub fn pro() -> Self {
        Self {
            name: "pro".to_string(),
            base_fee: dec!(29),
            included_tokens: 100_000_000, // 100M tokens/month
            price_per_million_tokens: dec!(0.30),
            included_gpu_hours: dec!(50),
            price_per_gpu_hour: dec!(1.20),
        }
    }

    /// Enterprise tier - custom pricing
    pub fn enterprise() -> Self {
        Self {
            name: "enterprise".to_string(),
            base_fee: dec!(299),
            included_tokens: 1_000_000_000, // 1B tokens/month
            price_per_million_tokens: dec!(0.10),
            included_gpu_hours: dec!(500),
            price_per_gpu_hour: dec!(0.80),
        }
    }
}

/// Usage summary for billing calculation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageSummary {
    /// Total tokens processed
    pub total_tokens: u64,
    /// Total GPU hours used
    pub total_gpu_hours: Decimal,
    /// Total API requests
    pub total_requests: u64,
    /// Breakdown by model
    pub by_model: std::collections::HashMap<String, ModelUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelUsage {
    pub tokens: u64,
    pub gpu_hours: Decimal,
    pub requests: u64,
}

/// Billing calculation result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BillingResult {
    /// Customer ID
    pub customer_id: String,
    /// Billing period start (Unix timestamp)
    pub period_start: u64,
    /// Billing period end (Unix timestamp)
    pub period_end: u64,
    /// Pricing tier used
    pub tier: String,
    /// Base subscription fee
    pub base_fee: Decimal,
    /// Token overage charge
    pub token_overage_cost: Decimal,
    /// Number of tokens over quota
    pub token_overage: u64,
    /// GPU overage charge
    pub gpu_overage_cost: Decimal,
    /// GPU hours over quota
    pub gpu_overage_hours: Decimal,
    /// Total amount due (USD)
    pub total_amount: Decimal,
    /// Usage summary
    pub usage: UsageSummary,
}

/// Pricing calculator
pub struct PricingCalculator {
    tiers: std::collections::HashMap<String, PricingTier>,
}

impl PricingCalculator {
    pub fn new() -> Self {
        let mut tiers = std::collections::HashMap::new();
        tiers.insert("free".to_string(), PricingTier::free());
        tiers.insert("starter".to_string(), PricingTier::starter());
        tiers.insert("pro".to_string(), PricingTier::pro());
        tiers.insert("enterprise".to_string(), PricingTier::enterprise());

        Self { tiers }
    }

    /// Calculate billing for a customer
    pub fn calculate(
        &self,
        customer_id: &str,
        tier_name: &str,
        usage: UsageSummary,
        period_start: u64,
        period_end: u64,
    ) -> Result<BillingResult> {
        let tier = self
            .tiers
            .get(tier_name)
            .ok_or_else(|| anyhow::anyhow!("Unknown pricing tier: {}", tier_name))?;

        // Calculate token overage
        let token_overage = usage.total_tokens.saturating_sub(tier.included_tokens);
        let token_overage_cost = if token_overage > 0 {
            let overage_millions = Decimal::from(token_overage) / dec!(1_000_000);
            overage_millions * tier.price_per_million_tokens
        } else {
            dec!(0)
        };

        // Calculate GPU overage
        let gpu_overage_hours = (usage.total_gpu_hours - tier.included_gpu_hours).max(dec!(0));
        let gpu_overage_cost = gpu_overage_hours * tier.price_per_gpu_hour;

        // Total
        let total_amount = tier.base_fee + token_overage_cost + gpu_overage_cost;

        Ok(BillingResult {
            customer_id: customer_id.to_string(),
            period_start,
            period_end,
            tier: tier_name.to_string(),
            base_fee: tier.base_fee,
            token_overage_cost,
            token_overage,
            gpu_overage_cost,
            gpu_overage_hours,
            total_amount,
            usage,
        })
    }

    /// Get pricing tier details
    pub fn get_tier(&self, tier_name: &str) -> Option<&PricingTier> {
        self.tiers.get(tier_name)
    }

    /// List all available tiers
    pub fn list_tiers(&self) -> Vec<&PricingTier> {
        self.tiers.values().collect()
    }
}

impl Default for PricingCalculator {
    fn default() -> Self {
        Self::new()
    }
}

/// Integration with Polar/Stripe
pub struct BillingIntegration {
    calculator: PricingCalculator,
    // In production, would have Polar/Stripe client
}

impl BillingIntegration {
    pub fn new() -> Self {
        Self {
            calculator: PricingCalculator::new(),
        }
    }

    /// Generate invoice for a billing period
    pub async fn generate_invoice(
        &self,
        customer_id: &str,
        tier: &str,
        usage: UsageSummary,
        period_start: u64,
        period_end: u64,
    ) -> Result<BillingResult> {
        let billing =
            self.calculator
                .calculate(customer_id, tier, usage, period_start, period_end)?;

        // In production, would:
        // 1. Create invoice in Polar
        // 2. Send usage data to Stripe
        // 3. Trigger email notification

        Ok(billing)
    }

    /// Get customer's current usage and projected cost
    pub async fn get_current_usage(&self, customer_id: &str, tier: &str) -> Result<BillingResult> {
        // In production, would fetch from usage tracker
        let usage = UsageSummary {
            total_tokens: 0,
            total_gpu_hours: dec!(0),
            total_requests: 0,
            by_model: std::collections::HashMap::new(),
        };

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        self.calculator
            .calculate(customer_id, tier, usage, now, now)
    }
}

impl Default for BillingIntegration {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_free_tier_no_overage() {
        let calc = PricingCalculator::new();
        let usage = UsageSummary {
            total_tokens: 500_000, // Under 1M included
            total_gpu_hours: dec!(0),
            total_requests: 100,
            by_model: std::collections::HashMap::new(),
        };

        let result = calc.calculate("cus_123", "free", usage, 0, 1000).unwrap();

        assert_eq!(result.base_fee, dec!(0));
        assert_eq!(result.token_overage_cost, dec!(0));
        assert_eq!(result.gpu_overage_cost, dec!(0));
        assert_eq!(result.total_amount, dec!(0));
    }

    #[test]
    fn test_starter_tier_token_overage() {
        let calc = PricingCalculator::new();
        let usage = UsageSummary {
            total_tokens: 15_000_000, // 5M over quota
            total_gpu_hours: dec!(2), // Under 5h included
            total_requests: 1000,
            by_model: std::collections::HashMap::new(),
        };

        let result = calc
            .calculate("cus_123", "starter", usage, 0, 1000)
            .unwrap();

        assert_eq!(result.base_fee, dec!(9));
        assert_eq!(result.token_overage, 5_000_000);
        // 5M tokens = 5 * $0.50 = $2.50
        assert_eq!(result.token_overage_cost, dec!(2.50));
        assert_eq!(result.gpu_overage_cost, dec!(0));
        assert_eq!(result.total_amount, dec!(11.50)); // $9 + $2.50
    }

    #[test]
    fn test_pro_tier_gpu_overage() {
        let calc = PricingCalculator::new();
        let usage = UsageSummary {
            total_tokens: 50_000_000,  // Under 100M included
            total_gpu_hours: dec!(60), // 10h over 50h quota
            total_requests: 5000,
            by_model: std::collections::HashMap::new(),
        };

        let result = calc.calculate("cus_123", "pro", usage, 0, 1000).unwrap();

        assert_eq!(result.base_fee, dec!(29));
        assert_eq!(result.token_overage_cost, dec!(0));
        assert_eq!(result.gpu_overage_hours, dec!(10));
        // 10h * $1.20 = $12.00
        assert_eq!(result.gpu_overage_cost, dec!(12));
        assert_eq!(result.total_amount, dec!(41)); // $29 + $12
    }

    #[test]
    fn test_enterprise_tier_both_overage() {
        let calc = PricingCalculator::new();
        let usage = UsageSummary {
            total_tokens: 1_200_000_000, // 200M over 1B quota
            total_gpu_hours: dec!(600),  // 100h over 500h quota
            total_requests: 100_000,
            by_model: std::collections::HashMap::new(),
        };

        let result = calc
            .calculate("cus_123", "enterprise", usage, 0, 1000)
            .unwrap();

        assert_eq!(result.base_fee, dec!(299));
        // 200M tokens * $0.10/M = $20.00
        assert_eq!(result.token_overage_cost, dec!(20));
        // 100h * $0.80 = $80.00
        assert_eq!(result.gpu_overage_cost, dec!(80));
        assert_eq!(result.total_amount, dec!(399)); // $299 + $20 + $80
    }

    #[test]
    fn test_list_tiers() {
        let calc = PricingCalculator::new();
        let tiers = calc.list_tiers();
        assert_eq!(tiers.len(), 4);

        let tier_names: Vec<&str> = tiers.iter().map(|t| t.name.as_str()).collect();
        assert!(tier_names.contains(&"free"));
        assert!(tier_names.contains(&"starter"));
        assert!(tier_names.contains(&"pro"));
        assert!(tier_names.contains(&"enterprise"));
    }

    #[test]
    fn test_invalid_tier() {
        let calc = PricingCalculator::new();
        let usage = UsageSummary {
            total_tokens: 0,
            total_gpu_hours: dec!(0),
            total_requests: 0,
            by_model: std::collections::HashMap::new(),
        };

        let result = calc.calculate("cus_123", "invalid", usage, 0, 1000);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Unknown pricing tier"));
    }
}
