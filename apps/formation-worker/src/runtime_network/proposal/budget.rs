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
            (1..=6).contains(&self.max_calls)
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
        guard.inspect(None, None)?;
        Ok(guard)
    }
    pub fn plan(&self) -> &BudgetPlan {
        &self.plan
    }
    pub fn reserve(&self, cell: &str) -> Result<()> {
        self.inspect(Some(cell), None)
    }
    /// Stop is operational and persistent across requester restarts. It never
    /// changes K or the Coordinator's existing closed error classes.
    pub fn halt(&self) -> Result<()> {
        self.inspect(None, Some(JournalEvent::Halt { halt: true }))
    }
    pub fn record_response(&self, response: ResponseEvidence) -> Result<()> {
        self.inspect(None, Some(JournalEvent::Response { response }))
    }
    fn inspect(&self, cell: Option<&str>, event: Option<JournalEvent>) -> Result<()> {
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
        let mut stopped = false;
        let mut responses = BTreeSet::new();
        for line in lines {
            match serde_json::from_str::<JournalEvent>(line)? {
                JournalEvent::Cell(id) => {
                    ensure!(cells.insert(id), "duplicate reservation");
                }
                JournalEvent::Halt { halt } => {
                    ensure!(halt, "invalid halt");
                    stopped = true;
                }
                JournalEvent::Response { response } => {
                    ensure!(
                        cells.contains(&response.cell) && responses.insert(response.cell.clone()),
                        "unclaimed or duplicate provider response"
                    );
                    stopped |= !response.within(&self.plan);
                }
            }
        }
        ensure!(
            cells.len() <= self.plan.max_calls as usize,
            "reservation count exceeded"
        );
        if let Some(JournalEvent::Response { response }) = &event {
            ensure!(
                cells.contains(&response.cell) && !responses.contains(&response.cell),
                "unclaimed or duplicate provider response"
            );
        }
        if let Some(event) = event {
            writeln!(f, "{}", serde_json::to_string(&event)?)?;
            f.sync_all()?;
        }
        if let Some(id) = cell {
            ensure!(!stopped, "provider protocol violation: run stopped");
            ensure!(
                cells.len() == responses.len(),
                "prior call response unresolved"
            );
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

/// Only closed finish categories, usage and a model-match boolean are stored;
/// never arbitrary vendor strings, reasoning, headers or raw error bodies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    Other,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseEvidence {
    pub cell: String,
    pub finish_reason: FinishReason,
    pub model_matches: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
}
impl ResponseEvidence {
    pub fn within(&self, plan: &BudgetPlan) -> bool {
        self.finish_reason == FinishReason::Stop
            && self.model_matches
            && self.input_tokens <= plan.input_token_cap
            && self.output_tokens <= plan.output_token_cap
    }
}
#[derive(Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum JournalEvent {
    Cell(String),
    Halt { halt: bool },
    Response { response: ResponseEvidence },
}
