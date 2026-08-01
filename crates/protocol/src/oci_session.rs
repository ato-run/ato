//! OCI session lifecycle domain types.
//!
//! An OCI (compose / docker-run / explicit-image) session is one that the CLI
//! runtime owns; a **host** (Desktop today, other runners later) observes it
//! through the `ato ps --json` projection and drives stop/restart through the
//! CLI. These are the safe, host-agnostic fields a supervising host reads —
//! they carry no CLI-internal runtime handles.
//!
//! `OciImportKind` and `OciSessionStatus` are part of the `ato ps --json` wire
//! shape (the `import_kind` / `status` fields); `OciSessionSnapshot` is the
//! normalized domain view a host builds from that projection. Single-sourced
//! here so the Desktop shell and the host-agnostic `runner` supervisor share
//! one definition rather than each carrying a mirror.

use serde::{Deserialize, Serialize};

/// How an OCI session's image/compose definition was imported.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum OciImportKind {
    Compose,
    DockerRunScript,
    ExplicitOci,
}

/// Lifecycle status of an OCI session as reported by the CLI projection.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OciSessionStatus {
    Running,
    Stopped,
    StopFailed,
}

/// Safe OCI session fields read from the CLI `ato ps --json` boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OciSessionSnapshot {
    pub id: String,
    pub import_kind: OciImportKind,
    pub status: OciSessionStatus,
    pub endpoint_url: Option<String>,
    pub service_count: usize,
    pub source_path: Option<String>,
    pub source_hash: Option<String>,
}
