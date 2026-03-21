mod heuristics;
mod json;
mod mapping;
mod types;

#[allow(unused_imports)]
pub(crate) use json::JsonErrorEnvelopeV1;
pub(crate) use mapping::{detect_command_context, from_anyhow, map_exit_code};
#[allow(unused_imports)]
pub(crate) use types::{CliDiagnostic, CliDiagnosticCode, CommandContext};

#[cfg(test)]
mod tests;
