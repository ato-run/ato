use serde::{Deserialize, Serialize};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// 全ランタイム共通の正規化された計測データ。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedMetrics {
    pub session_id: String,
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub resources: ResourceStats,
    pub metadata: RuntimeMetadata,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResourceStats {
    pub duration_ms: u128,
    pub cpu_seconds: f64,
    pub peak_memory_bytes: u64,
    pub net_egress_bytes: Option<u64>,
    pub gpu_seconds: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "runtime_type", content = "details")]
pub enum RuntimeMetadata {
    Nacelle {
        pid: u32,
        exit_code: Option<i32>,
    },
    Oci {
        container_id: String,
        image_hash: String,
        exit_code: Option<i32>,
    },
    Wasm {
        module_hash: String,
        engine: String,
    },
}

/// 計測セッションの基礎情報と壁時計タイマー。
#[derive(Clone)]
pub struct MetricsSession {
    session_id: String,
    started_at: u64,
    started_at_instant: Instant,
}

impl MetricsSession {
    pub fn new(session_id: impl Into<String>) -> Self {
        let started_at = now_unix_secs();
        Self {
            session_id: session_id.into(),
            started_at,
            started_at_instant: Instant::now(),
        }
    }

    pub fn started_at(&self) -> u64 {
        self.started_at
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn elapsed_ms(&self) -> u128 {
        self.started_at_instant.elapsed().as_millis()
    }

    pub fn snapshot(&self, resources: ResourceStats, metadata: RuntimeMetadata) -> UnifiedMetrics {
        self.build(resources, metadata, None)
    }

    pub fn finalize(&self, resources: ResourceStats, metadata: RuntimeMetadata) -> UnifiedMetrics {
        self.build(resources, metadata, Some(now_unix_secs()))
    }

    fn build(
        &self,
        mut resources: ResourceStats,
        metadata: RuntimeMetadata,
        ended_at: Option<u64>,
    ) -> UnifiedMetrics {
        if resources.duration_ms == 0 {
            resources.duration_ms = self.elapsed_ms();
        }

        UnifiedMetrics {
            session_id: self.session_id.clone(),
            started_at: self.started_at,
            ended_at,
            resources,
            metadata,
        }
    }
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
