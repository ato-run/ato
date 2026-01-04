//! Job History persistence module (UARC V1.1.0).
//!
//! This module provides persistent storage for job execution history,
//! enabling gRPC queries for job status after Coordinator dependency removal.

mod store;

pub use store::{
    JobHistory, JobHistoryError, JobHistoryStore, JobPhase, JobRecord, SqliteJobHistoryStore,
};
