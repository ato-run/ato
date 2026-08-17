//! Process-to-process DTOs for computation-oriented product shells.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum ComputationCommand {
    Init {
        capsule: String,
        initial_only: bool,
    },
    Resume {
        selector: String,
        branch: Option<String>,
    },
    Stop {
        capsule: String,
    },
    Encap {
        selector: String,
        output: String,
    },
    RunPortable {
        capsule_file: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputationCommandResult {
    pub success: bool,
    pub output: String,
}
