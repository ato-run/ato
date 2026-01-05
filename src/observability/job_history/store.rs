//! SQLite-based Job History Store for UARC V1.1.0.
//!
//! Provides persistent storage for job execution records, enabling
//! pull-based job status queries via gRPC without Coordinator dependency.

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Mutex;
use tracing::{debug, info};

/// Errors from job history operations
#[derive(Debug, thiserror::Error)]
pub enum JobHistoryError {
    #[error("Database error: {0}")]
    DatabaseError(String),

    #[error("Job not found: {0}")]
    NotFound(String),

    #[error("Invalid phase transition: {from:?} -> {to:?}")]
    InvalidTransition { from: JobPhase, to: JobPhase },

    #[error("Serialization error: {0}")]
    SerializationError(String),
}

impl From<rusqlite::Error> for JobHistoryError {
    fn from(e: rusqlite::Error) -> Self {
        JobHistoryError::DatabaseError(e.to_string())
    }
}

/// Job execution phases per UARC V1.1.0
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobPhase {
    /// Job is queued but not yet started
    Pending,
    /// Job is currently running
    Running,
    /// Job completed successfully
    Succeeded,
    /// Job failed with an error
    Failed,
    /// Job was cancelled by user/system
    Cancelled,
}

impl JobPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobPhase::Pending => "pending",
            JobPhase::Running => "running",
            JobPhase::Succeeded => "succeeded",
            JobPhase::Failed => "failed",
            JobPhase::Cancelled => "cancelled",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "pending" => Some(JobPhase::Pending),
            "running" => Some(JobPhase::Running),
            "succeeded" => Some(JobPhase::Succeeded),
            "failed" => Some(JobPhase::Failed),
            "cancelled" => Some(JobPhase::Cancelled),
            _ => None,
        }
    }

    /// Check if this phase represents a terminal state
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            JobPhase::Succeeded | JobPhase::Failed | JobPhase::Cancelled
        )
    }
}

/// A job execution record
#[derive(Debug, Clone)]
pub struct JobRecord {
    /// Unique job identifier (typically UUID)
    pub job_id: String,
    /// Capsule name
    pub capsule_name: String,
    /// Capsule version
    pub capsule_version: String,
    /// Current execution phase
    pub phase: JobPhase,
    /// Error message if phase is Failed
    pub error_message: Option<String>,
    /// Exit code if job has completed
    pub exit_code: Option<i32>,
    /// Time when job was created
    pub created_at: DateTime<Utc>,
    /// Time when job started running
    pub started_at: Option<DateTime<Utc>>,
    /// Time when job finished (succeeded/failed/cancelled)
    pub finished_at: Option<DateTime<Utc>>,
    /// Execution duration in seconds
    pub duration_secs: Option<u64>,
    /// Resource usage summary (JSON serialized)
    pub resource_usage_json: Option<String>,
}

/// Trait for job history storage backends
pub trait JobHistory: Send + Sync {
    /// Insert a new job record
    fn insert_job(&self, record: &JobRecord) -> Result<(), JobHistoryError>;

    /// Update an existing job's phase
    fn update_phase(
        &self,
        job_id: &str,
        phase: JobPhase,
        error_message: Option<&str>,
        exit_code: Option<i32>,
    ) -> Result<(), JobHistoryError>;

    /// Get a job by ID
    fn get_job(&self, job_id: &str) -> Result<JobRecord, JobHistoryError>;

    /// List recent jobs, optionally filtered by capsule name
    fn list_jobs(
        &self,
        capsule_name: Option<&str>,
        limit: usize,
    ) -> Result<Vec<JobRecord>, JobHistoryError>;

    /// Cleanup old records (older than retention_days)
    fn cleanup_old_records(&self, retention_days: u32) -> Result<u64, JobHistoryError>;
}

/// SQLite-backed implementation of JobHistory
pub struct SqliteJobHistoryStore {
    conn: Mutex<Connection>,
}

impl SqliteJobHistoryStore {
    /// Create a new SQLite job history store
    pub fn new(db_path: &Path) -> Result<Self, JobHistoryError> {
        let conn = Connection::open(db_path)?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.init_schema()?;
        Ok(store)
    }

    /// Create an in-memory store (for testing)
    pub fn in_memory() -> Result<Self, JobHistoryError> {
        let conn = Connection::open_in_memory()?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.init_schema()?;
        Ok(store)
    }

    fn init_schema(&self) -> Result<(), JobHistoryError> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS job_history (
                job_id TEXT PRIMARY KEY,
                capsule_name TEXT NOT NULL,
                capsule_version TEXT NOT NULL,
                phase TEXT NOT NULL,
                error_message TEXT,
                exit_code INTEGER,
                created_at TEXT NOT NULL,
                started_at TEXT,
                finished_at TEXT,
                duration_secs INTEGER,
                resource_usage_json TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_job_history_capsule ON job_history(capsule_name);
            CREATE INDEX IF NOT EXISTS idx_job_history_created ON job_history(created_at);
            CREATE INDEX IF NOT EXISTS idx_job_history_phase ON job_history(phase);
            "#,
        )?;
        info!("Job history database schema initialized");
        Ok(())
    }
}

impl JobHistory for SqliteJobHistoryStore {
    fn insert_job(&self, record: &JobRecord) -> Result<(), JobHistoryError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            r#"
            INSERT INTO job_history (
                job_id, capsule_name, capsule_version, phase, error_message, exit_code,
                created_at, started_at, finished_at, duration_secs, resource_usage_json
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11
            )
            "#,
            params![
                record.job_id,
                record.capsule_name,
                record.capsule_version,
                record.phase.as_str(),
                record.error_message,
                record.exit_code,
                record.created_at.to_rfc3339(),
                record.started_at.map(|t| t.to_rfc3339()),
                record.finished_at.map(|t| t.to_rfc3339()),
                record.duration_secs,
                record.resource_usage_json,
            ],
        )?;
        debug!("Inserted job record: {}", record.job_id);
        Ok(())
    }

    fn update_phase(
        &self,
        job_id: &str,
        phase: JobPhase,
        error_message: Option<&str>,
        exit_code: Option<i32>,
    ) -> Result<(), JobHistoryError> {
        let conn = self.conn.lock().unwrap();
        let now = Utc::now();

        // Calculate duration if transitioning to terminal state
        let mut stmt = conn.prepare("SELECT started_at FROM job_history WHERE job_id = ?1")?;
        let started_at: Option<String> = stmt.query_row(params![job_id], |row| row.get(0)).ok();

        let duration_secs = if phase.is_terminal() {
            started_at.as_ref().and_then(|s| {
                DateTime::parse_from_rfc3339(s)
                    .ok()
                    .map(|start| (now - start.with_timezone(&Utc)).num_seconds().max(0) as u64)
            })
        } else {
            None
        };

        let rows_affected = if phase.is_terminal() {
            conn.execute(
                r#"
                UPDATE job_history 
                SET phase = ?1, error_message = ?2, exit_code = ?3, 
                    finished_at = ?4, duration_secs = ?5
                WHERE job_id = ?6
                "#,
                params![
                    phase.as_str(),
                    error_message,
                    exit_code,
                    now.to_rfc3339(),
                    duration_secs,
                    job_id,
                ],
            )?
        } else if phase == JobPhase::Running {
            conn.execute(
                r#"
                UPDATE job_history 
                SET phase = ?1, started_at = ?2
                WHERE job_id = ?3
                "#,
                params![phase.as_str(), now.to_rfc3339(), job_id],
            )?
        } else {
            conn.execute(
                r#"
                UPDATE job_history SET phase = ?1 WHERE job_id = ?2
                "#,
                params![phase.as_str(), job_id],
            )?
        };

        if rows_affected == 0 {
            return Err(JobHistoryError::NotFound(job_id.to_string()));
        }

        debug!("Updated job {} to phase {:?}", job_id, phase);
        Ok(())
    }

    fn get_job(&self, job_id: &str) -> Result<JobRecord, JobHistoryError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            r#"
            SELECT job_id, capsule_name, capsule_version, phase, error_message, exit_code,
                   created_at, started_at, finished_at, duration_secs, resource_usage_json
            FROM job_history
            WHERE job_id = ?1
            "#,
        )?;

        stmt.query_row(params![job_id], |row| {
            Ok(JobRecord {
                job_id: row.get(0)?,
                capsule_name: row.get(1)?,
                capsule_version: row.get(2)?,
                phase: JobPhase::from_str(row.get::<_, String>(3)?.as_str())
                    .unwrap_or(JobPhase::Pending),
                error_message: row.get(4)?,
                exit_code: row.get(5)?,
                created_at: parse_datetime(&row.get::<_, String>(6)?),
                started_at: row.get::<_, Option<String>>(7)?.map(|s| parse_datetime(&s)),
                finished_at: row.get::<_, Option<String>>(8)?.map(|s| parse_datetime(&s)),
                duration_secs: row.get(9)?,
                resource_usage_json: row.get(10)?,
            })
        })
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => JobHistoryError::NotFound(job_id.to_string()),
            other => JobHistoryError::from(other),
        })
    }

    fn list_jobs(
        &self,
        capsule_name: Option<&str>,
        limit: usize,
    ) -> Result<Vec<JobRecord>, JobHistoryError> {
        let conn = self.conn.lock().unwrap();

        let mut jobs = Vec::new();

        if let Some(name) = capsule_name {
            let mut stmt = conn.prepare(
                r#"
                SELECT job_id, capsule_name, capsule_version, phase, error_message, exit_code,
                       created_at, started_at, finished_at, duration_secs, resource_usage_json
                FROM job_history
                WHERE capsule_name = ?1
                ORDER BY created_at DESC
                LIMIT ?2
                "#,
            )?;

            let rows = stmt.query_map(params![name, limit as i64], |row| parse_job_row(row))?;

            for row_result in rows {
                jobs.push(row_result?);
            }
        } else {
            let mut stmt = conn.prepare(
                r#"
                SELECT job_id, capsule_name, capsule_version, phase, error_message, exit_code,
                       created_at, started_at, finished_at, duration_secs, resource_usage_json
                FROM job_history
                ORDER BY created_at DESC
                LIMIT ?1
                "#,
            )?;

            let rows = stmt.query_map(params![limit as i64], |row| parse_job_row(row))?;

            for row_result in rows {
                jobs.push(row_result?);
            }
        };

        Ok(jobs)
    }

    fn cleanup_old_records(&self, retention_days: u32) -> Result<u64, JobHistoryError> {
        let conn = self.conn.lock().unwrap();
        let cutoff = Utc::now() - chrono::Duration::days(retention_days as i64);
        let cutoff_str = cutoff.to_rfc3339();

        let deleted = conn.execute(
            "DELETE FROM job_history WHERE created_at < ?1 AND phase IN ('succeeded', 'failed', 'cancelled')",
            params![cutoff_str],
        )?;

        if deleted > 0 {
            info!("Cleaned up {} old job records", deleted);
        }
        Ok(deleted as u64)
    }
}

/// Type alias for backward compatibility
pub type JobHistoryStore = SqliteJobHistoryStore;

fn parse_datetime(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

fn parse_job_row(row: &rusqlite::Row) -> Result<JobRecord, rusqlite::Error> {
    Ok(JobRecord {
        job_id: row.get(0)?,
        capsule_name: row.get(1)?,
        capsule_version: row.get(2)?,
        phase: JobPhase::from_str(row.get::<_, String>(3)?.as_str()).unwrap_or(JobPhase::Pending),
        error_message: row.get(4)?,
        exit_code: row.get(5)?,
        created_at: parse_datetime(&row.get::<_, String>(6)?),
        started_at: row.get::<_, Option<String>>(7)?.map(|s| parse_datetime(&s)),
        finished_at: row.get::<_, Option<String>>(8)?.map(|s| parse_datetime(&s)),
        duration_secs: row.get(9)?,
        resource_usage_json: row.get(10)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_and_get_job() {
        let store = SqliteJobHistoryStore::in_memory().unwrap();

        let record = JobRecord {
            job_id: "test-job-1".to_string(),
            capsule_name: "my-capsule".to_string(),
            capsule_version: "1.0.0".to_string(),
            phase: JobPhase::Pending,
            error_message: None,
            exit_code: None,
            created_at: Utc::now(),
            started_at: None,
            finished_at: None,
            duration_secs: None,
            resource_usage_json: None,
        };

        store.insert_job(&record).unwrap();

        let retrieved = store.get_job("test-job-1").unwrap();
        assert_eq!(retrieved.capsule_name, "my-capsule");
        assert_eq!(retrieved.phase, JobPhase::Pending);
    }

    #[test]
    fn test_update_phase() {
        let store = SqliteJobHistoryStore::in_memory().unwrap();

        let record = JobRecord {
            job_id: "test-job-2".to_string(),
            capsule_name: "test-capsule".to_string(),
            capsule_version: "1.0.0".to_string(),
            phase: JobPhase::Pending,
            error_message: None,
            exit_code: None,
            created_at: Utc::now(),
            started_at: None,
            finished_at: None,
            duration_secs: None,
            resource_usage_json: None,
        };

        store.insert_job(&record).unwrap();
        store
            .update_phase("test-job-2", JobPhase::Running, None, None)
            .unwrap();

        let updated = store.get_job("test-job-2").unwrap();
        assert_eq!(updated.phase, JobPhase::Running);
        assert!(updated.started_at.is_some());
    }

    #[test]
    fn test_phase_terminal_sets_finished() {
        let store = SqliteJobHistoryStore::in_memory().unwrap();

        let record = JobRecord {
            job_id: "test-job-3".to_string(),
            capsule_name: "test-capsule".to_string(),
            capsule_version: "1.0.0".to_string(),
            phase: JobPhase::Running,
            error_message: None,
            exit_code: None,
            created_at: Utc::now(),
            started_at: Some(Utc::now()),
            finished_at: None,
            duration_secs: None,
            resource_usage_json: None,
        };

        store.insert_job(&record).unwrap();
        store
            .update_phase("test-job-3", JobPhase::Succeeded, None, Some(0))
            .unwrap();

        let updated = store.get_job("test-job-3").unwrap();
        assert_eq!(updated.phase, JobPhase::Succeeded);
        assert!(updated.finished_at.is_some());
        assert_eq!(updated.exit_code, Some(0));
    }

    #[test]
    fn test_list_jobs() {
        let store = SqliteJobHistoryStore::in_memory().unwrap();

        for i in 1..=5 {
            let record = JobRecord {
                job_id: format!("job-{}", i),
                capsule_name: "test-capsule".to_string(),
                capsule_version: "1.0.0".to_string(),
                phase: JobPhase::Pending,
                error_message: None,
                exit_code: None,
                created_at: Utc::now(),
                started_at: None,
                finished_at: None,
                duration_secs: None,
                resource_usage_json: None,
            };
            store.insert_job(&record).unwrap();
        }

        let jobs = store.list_jobs(Some("test-capsule"), 3).unwrap();
        assert_eq!(jobs.len(), 3);
    }

    #[test]
    fn test_job_not_found() {
        let store = SqliteJobHistoryStore::in_memory().unwrap();
        let result = store.get_job("nonexistent");
        assert!(matches!(result, Err(JobHistoryError::NotFound(_))));
    }
}
