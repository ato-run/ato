use axum::{
    extract::{Json, Path, State},
    http::{HeaderValue, Method, StatusCode},
    response::{IntoResponse, Sse},
    routing::{delete, get, post},
    Router,
};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tower_http::cors::CorsLayer;
use tracing::{error, info};

use crate::auth::AuthManager;
use crate::capsule_manager::{CapsuleManager, DeployCapsuleRequest};
use crate::hardware::GpuDetector;
use crate::manifest::{Manifest, Resource};
use crate::metrics::collector::MetricsCollector;
use crate::network::service_registry::ServiceRegistry;
use capsule_core::capsule_v1::{
    CapsuleExecution, CapsuleManifestV1, CapsuleRequirements, CapsuleRouting, CapsuleType,
    RuntimeType,
};

#[derive(Clone)]
pub struct AppState {
    pub capsule_manager: Arc<CapsuleManager>,
    pub service_registry: Arc<ServiceRegistry>,
    pub gpu_detector: Arc<dyn GpuDetector>,
    pub auth_manager: Arc<AuthManager>,
    pub metrics_collector: Option<Arc<MetricsCollector>>,
}

#[derive(Serialize)]
struct StatusResponse {
    status: String,
    version: String,
    gpu_info: Option<GpuInfo>,
    capsules: Vec<CapsuleInfo>,
}

#[derive(Serialize)]
struct GpuInfo {
    count: usize,
    total_vram_gb: f64,
    names: Vec<String>,
}

#[derive(Serialize)]
struct CapsuleInfo {
    id: String,
    status: String,
    local_url: Option<String>,
    port: Option<u16>,
    uptime: Option<u64>,
}

#[derive(Deserialize)]
struct ApplyRequest {
    // HCL content as string
    hcl: String,
}

#[derive(Serialize)]
struct ApplyResponse {
    capsule_id: String,
    status: String,
    local_url: String,
}

pub async fn start_api_server(
    port: u16,
    capsule_manager: Arc<CapsuleManager>,
    service_registry: Arc<ServiceRegistry>,
    gpu_detector: Arc<dyn GpuDetector>,
    auth_manager: Arc<AuthManager>,
    metrics_collector: Option<Arc<MetricsCollector>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let state = AppState {
        capsule_manager,
        service_registry,
        gpu_detector,
        auth_manager: auth_manager.clone(),
        metrics_collector,
    };

    // CORS configuration (Strict)
    let cors = CorsLayer::new()
        .allow_origin([
            "http://localhost:3000".parse::<HeaderValue>().unwrap(),
            "https://app.gumball.net".parse::<HeaderValue>().unwrap(),
        ])
        .allow_methods([Method::GET, Method::POST, Method::DELETE, Method::OPTIONS])
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::AUTHORIZATION,
        ]);

    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/metrics", get(metrics_handler))
        .route("/v1/status", get(status_handler))
        .route("/v1/apply", post(apply_handler))
        .route("/v1/destroy/:id", delete(destroy_handler))
        .route("/v1/logs/:id", get(logs_handler))
        .layer(axum::middleware::from_fn(move |req, next| {
            crate::auth::auth_middleware(auth_manager.clone(), req, next)
        }))
        .layer(cors)
        .with_state(state);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    info!("REST API server listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn health_handler() -> impl IntoResponse {
    StatusCode::OK
}

/// Prometheus metrics endpoint for pull-based observability
async fn metrics_handler(State(state): State<AppState>) -> impl IntoResponse {
    match &state.metrics_collector {
        Some(collector) => match collector.gather_prometheus() {
            Ok(metrics_text) => (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                metrics_text,
            )
                .into_response(),
            Err(e) => {
                error!("Failed to gather Prometheus metrics: {}", e);
                (StatusCode::INTERNAL_SERVER_ERROR, "Failed to gather metrics").into_response()
            }
        },
        None => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "# Metrics collector not configured\n".to_string(),
        )
            .into_response(),
    }
}

async fn status_handler(State(state): State<AppState>) -> impl IntoResponse {
    info!("Handling status request - returning local_url");
    let gpu_report = state.gpu_detector.detect_gpus().ok();

    let gpu_info = gpu_report.map(|report| GpuInfo {
        count: report.gpus.len(),
        total_vram_gb: report.total_vram_gb(),
        names: report.gpus.iter().map(|g| g.device_name.clone()).collect(),
    });

    let capsules = state
        .capsule_manager
        .list_capsules()
        .unwrap_or_default()
        .into_iter()
        .map(|c| {
            let service = state
                .service_registry
                .get_services()
                .iter()
                .find(|s| s.name == c.id)
                .cloned();

            let url = c.remote_url.clone().or_else(|| {
                service
                    .as_ref()
                    .map(|s| format!("http://localhost:{}", s.port))
            });
            let port = service.as_ref().map(|s| s.port);

            let uptime = c.started_at.map(|start| {
                std::time::SystemTime::now()
                    .duration_since(start)
                    .unwrap_or_default()
                    .as_secs()
            });

            CapsuleInfo {
                id: c.id,
                status: c.status.to_string(),
                local_url: url,
                port,
                uptime,
            }
        })
        .collect();

    Json(StatusResponse {
        status: "ready".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        gpu_info,
        capsules,
    })
}

async fn apply_handler(
    State(state): State<AppState>,
    Json(payload): Json<ApplyRequest>,
) -> impl IntoResponse {
    // 1. Parse HCL
    let manifest_hcl: Manifest = match hcl::from_str(&payload.hcl) {
        Ok(m) => m,
        Err(e) => {
            return Json(ApplyResponse {
                capsule_id: "".to_string(),
                status: format!("HCL Parse Error: {}", e),
                local_url: "".to_string(),
            })
        }
    };

    // 2. Convert to CapsuleManifestV1 (Best Effort)
    // Find the first container resource
    let (capsule_id, container_config) = match manifest_hcl.resource.get("container") {
        Some(containers) => {
            if let Some((name, Resource::Container(config))) = containers.iter().next() {
                (name.clone(), config)
            } else {
                return Json(ApplyResponse {
                    capsule_id: "".to_string(),
                    status: "No container resource found in HCL".to_string(),
                    local_url: "".to_string(),
                });
            }
        }
        None => {
            return Json(ApplyResponse {
                capsule_id: "".to_string(),
                status: "No container resource found in HCL".to_string(),
                local_url: "".to_string(),
            })
        }
    };

    // Check for compute resources
    let compute_res = manifest_hcl
        .resource
        .get("compute")
        .and_then(|c| c.values().next())
        .and_then(|r| match r {
            Resource::Compute(c) => Some(c),
            _ => None,
        });

    let vram_string = compute_res.and_then(|c| c.vram_min.as_ref()).cloned();

    // Map Native vs Docker
    let (runtime_type, entrypoint) = if let Some(native_cfg) = &container_config.native {
        (RuntimeType::Native, native_cfg.runtime.clone())
        // TODO: What about native_cfg.args?
        // Use the shell_words join or assume entrypoint has it?
        // In runplan we assume binary_path has it.
        // Here HCL has `runtime` and `args` (Vec<String>).
        // We should join them: "runtime arg1 arg2"
        // let full_cmd = format!("{} {}", native_cfg.runtime, native_cfg.args.join(" "));
        // (RuntimeType::Native, full_cmd)
    } else {
        (RuntimeType::Docker, container_config.image.clone())
    };

    let manifest = CapsuleManifestV1 {
        schema_version: "1.0".to_string(),
        name: capsule_id.clone(),
        version: "0.0.1".to_string(),
        capsule_type: CapsuleType::App,
        metadata: Default::default(),
        capabilities: None,
        requirements: CapsuleRequirements {
            platform: vec![],
            vram_min: vram_string,
            vram_recommended: None,
            disk: None,
            dependencies: vec![],
        },
        execution: CapsuleExecution {
            runtime: runtime_type,
            entrypoint,
            port: None, // HCL doesn't seem to have explicit port field in container block?
            health_check: None,
            startup_timeout: 60,
            env: container_config
                .env
                .clone()
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default(),
            signals: Default::default(),
        },
        storage: Default::default(),
        routing: CapsuleRouting::default(),
        network: None,
        model: None,
        transparency: None,
        pool: None,
        targets: None,
    };

    // 3. Deploy - pass manifest directly (no JSON serialization!)
    let request = DeployCapsuleRequest {
        capsule_id: capsule_id.clone(),
        manifest,
        raw_manifest_bytes: None, // HCL-generated manifests have no external signature
        oci_image: container_config.image.clone(),
        digest: String::new(),
        extra_args: None,
        signature: None,
    };

    match state.capsule_manager.deploy_capsule(request).await {
        Ok(status) => {
            let url = state
                .service_registry
                .get_services()
                .iter()
                .find(|s| s.name == capsule_id)
                .map(|s| format!("http://localhost:{}", s.port))
                .unwrap_or_else(|| "http://localhost".to_string());

            Json(ApplyResponse {
                capsule_id,
                status,
                local_url: url,
            })
        }
        Err(e) => {
            error!("deploy_capsule returned error: {}", e);
            Json(ApplyResponse {
                capsule_id,
                status: format!("error: {}", e),
                local_url: "".to_string(),
            })
        }
    }
}

async fn destroy_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.capsule_manager.stop_capsule(&id).await {
        Ok(scrubbed) => {
            Json(serde_json::json!({ "status": "destroyed", "id": id, "vram_scrubbed": scrubbed }))
        }
        Err(e) => Json(serde_json::json!({ "status": "error", "message": e.to_string() })),
    }
}

async fn logs_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Sse<impl Stream<Item = Result<axum::response::sse::Event, axum::Error>>> {
    let log_path = state.capsule_manager.get_capsule_log_path(&id);

    let stream = async_stream::stream! {
        if let Some(path_str) = log_path {
            let path = std::path::PathBuf::from(path_str);

            // Wait for file
            let mut file = None;
            for _ in 0..20 {
                match tokio::fs::File::open(&path).await {
                    Ok(f) => {
                        file = Some(f);
                        break;
                    }
                    Err(_) => {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        yield Ok(axum::response::sse::Event::default().comment("waiting for log file"));
                    }
                }
            }

            if let Some(mut f) = file {
                use tokio::io::AsyncReadExt;
                let mut buffer = [0; 1024];
                loop {
                    match f.read(&mut buffer).await {
                        Ok(0) => {
                            tokio::time::sleep(Duration::from_millis(500)).await;
                            continue;
                        }
                        Ok(n) => {
                            let chunk = String::from_utf8_lossy(&buffer[..n]);
                            for line in chunk.lines() {
                                if !line.trim().is_empty() {
                                    let sanitized = line.replace(['\n', '\r'], " ");
                                    yield Ok(axum::response::sse::Event::default().data(sanitized));
                                }
                            }
                        }
                        Err(e) => {
                             let error_msg = format!("Error reading logs: {}", e);
                             yield Ok(axum::response::sse::Event::default().data(error_msg));
                             break;
                        }
                    }
                }
            } else {
                 yield Ok(axum::response::sse::Event::default().data("Timed out waiting for log file creation"));
            }
        } else {
            yield Ok(axum::response::sse::Event::default().data("Log path not found for capsule"));
        }
    };

    Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}
