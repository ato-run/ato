//! Acceptance-side dollar reservation journal, separate from Formation budgets.
//! A reservation is consumed before credentials are read. Never refunded on
//! timeout/unknown usage. Incomplete writes fail closed; no implicit reset.
use anyhow::{Result, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::OpenOptions,
    io::{Read, Write},
    path::{Path, PathBuf},
};

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
        let numerator = (u128::from(input) * u128::from(self.input_price))
            .checked_add(u128::from(output) * u128::from(self.output_price))
            .ok_or_else(|| anyhow::anyhow!("cost overflow"))?;
        Ok(u64::try_from(numerator.div_ceil(1_000_000))?)
    }
    pub fn validate(&self) -> Result<u64> {
        ensure!(
            self.max_calls > 0
                && self.input_token_cap > 0
                && (1..=2048).contains(&self.output_token_cap)
                && self.input_price > 0
                && self.output_price > 0
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
pub struct CallBudget {
    path: PathBuf,
    plan: BudgetPlan,
}
impl CallBudget {
    /// Explicit initialization only. Existing journals are never overwritten.
    pub fn create(path: &Path, plan: BudgetPlan) -> Result<Self> {
        plan.validate()?;
        let mut f = OpenOptions::new().create_new(true).write(true).open(path)?;
        writeln!(f, "{}", serde_json::to_string(&plan)?)?;
        f.sync_all()?;
        Ok(Self {
            path: path.into(),
            plan,
        })
    }
    pub fn reopen(path: &Path, plan: BudgetPlan) -> Result<Self> {
        plan.validate()?;
        ensure!(path.is_file(), "reservation journal missing");
        let guard = Self {
            path: path.into(),
            plan,
        };
        guard.inspect(None)?;
        Ok(guard)
    }
    pub fn plan(&self) -> &BudgetPlan {
        &self.plan
    }
    pub fn reserve(&self, cell: &str) -> Result<()> {
        self.inspect(Some(cell))
    }
    fn inspect(&self, cell: Option<&str>) -> Result<()> {
        self.plan.validate()?;
        let mut f = OpenOptions::new()
            .read(true)
            .append(true)
            .open(&self.path)?;
        f.lock_exclusive()?;
        let mut bytes = String::new();
        Read::by_ref(&mut f)
            .take(64 * 1024 + 1)
            .read_to_string(&mut bytes)?;
        ensure!(
            bytes.len() <= 64 * 1024 && bytes.ends_with('\n'),
            "corrupt reservation journal"
        );
        let mut lines = bytes.lines();
        let saved: BudgetPlan = serde_json::from_str(
            lines
                .next()
                .ok_or_else(|| anyhow::anyhow!("missing plan"))?,
        )?;
        ensure!(saved == self.plan, "reservation plan changed");
        let mut cells = BTreeSet::new();
        for line in lines {
            let id: String = serde_json::from_str(line)?;
            ensure!(cells.insert(id), "duplicate reservation");
        }
        ensure!(
            cells.len() <= self.plan.max_calls as usize,
            "reservation count exceeded"
        );
        if let Some(id) = cell {
            ensure!(
                !id.is_empty()
                    && id.len() <= 128
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
                "invalid cell"
            );
            ensure!(
                !cells.contains(id) && cells.len() < self.plan.max_calls as usize,
                "cell already consumed or call budget exhausted"
            );
            writeln!(f, "{}", serde_json::to_string(id)?)?;
            f.sync_all()?;
        }
        Ok(()) // File drop releases cross-process lock, including every error.
    }
}
