//! Process-to-process DTOs for computation-oriented product shells.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum ComputationCommand {
    Run { source: String },
    ListRuns,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputationCommandResult {
    pub success: bool,
    pub output: String,
}
