use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::application::pipeline::producer::PublishDryRunStageResult;
use crate::application::ports::publish::{
    DestinationSpec, PublishableArtifact, PublishedLocation, SharedDestinationPort,
};

pub type PrivatePublishResult = crate::publish_private::PublishPrivateResult;
pub type PrivatePublishSummary = crate::publish_private::PublishPrivateSummary;

#[derive(Debug, Clone)]
pub struct PrivatePublishRequest {
    pub registry_url: String,
    pub publisher_hint: Option<String>,
    pub artifact_path: Option<PathBuf>,
    pub force_large_payload: bool,
    pub scoped_id: Option<String>,
    pub allow_existing: bool,
}

#[derive(Debug, Clone)]
pub struct OfficialPublishRequest<'a> {
    pub cwd: &'a Path,
    pub registry_url: &'a str,
    pub fix: bool,
}

#[derive(Debug, Clone)]
pub struct OfficialPublishOutcome {
    pub route: crate::publish_official::PublishRoutePlan,
    pub fix_result: crate::publish_official::WorkflowFixResult,
    pub diagnosis: crate::publish_official::OfficialPublishDiagnosis,
}

#[derive(Debug, Clone)]
pub struct PublishPhaseRequest {
    pub artifact: PublishableArtifact,
    pub destination: DestinationSpec,
}

pub struct PublishPhase {
    destination: SharedDestinationPort,
}

impl PublishPhase {
    pub fn new(destination: SharedDestinationPort) -> Self {
        Self { destination }
    }

    pub async fn execute(&self, request: &PublishPhaseRequest) -> Result<PublishedLocation> {
        self.destination
            .publish(&request.artifact, &request.destination)
            .await
    }
}

pub fn summarize_private_publish(request: &PrivatePublishRequest) -> Result<PrivatePublishSummary> {
    crate::publish_private::summarize(&crate::publish_private::PublishPrivateArgs {
        registry_url: request.registry_url.clone(),
        publisher_hint: request.publisher_hint.clone(),
        artifact_path: request.artifact_path.clone(),
        force_large_payload: request.force_large_payload,
        scoped_id: request.scoped_id.clone(),
        allow_existing: request.allow_existing,
    })
}

pub fn run_private_publish_phase(request: PrivatePublishRequest) -> Result<PrivatePublishResult> {
    crate::publish_private::execute(crate::publish_private::PublishPrivateArgs {
        registry_url: request.registry_url,
        publisher_hint: request.publisher_hint,
        artifact_path: request.artifact_path,
        force_large_payload: request.force_large_payload,
        scoped_id: request.scoped_id,
        allow_existing: request.allow_existing,
    })
}

pub fn run_official_publish_phase(
    request: &OfficialPublishRequest<'_>,
) -> Result<OfficialPublishOutcome> {
    let route = crate::publish_official::build_route_plan(request.registry_url);

    let mut fix_result = crate::publish_official::WorkflowFixResult::default();
    let mut diagnosis =
        crate::publish_official::diagnose_official(request.cwd, request.registry_url);
    if request.fix && diagnosis.needs_workflow_fix {
        fix_result = crate::publish_official::apply_workflow_fix_once(request.cwd)
            .with_context(|| "Failed to apply official publish workflow fix")?;
        diagnosis = crate::publish_official::diagnose_official(request.cwd, request.registry_url);
    }

    Ok(OfficialPublishOutcome {
        route,
        fix_result,
        diagnosis,
    })
}

pub fn official_publish_diagnosis_lines(outcome: &OfficialPublishOutcome) -> Vec<String> {
    let mut lines = vec![format!(
        "🔎 official publish route registry={} route={:?}",
        outcome.route.registry_url, outcome.route.route
    )];
    lines.extend(outcome.diagnosis.stages.iter().map(|stage| {
        let icon = if stage.ok { "✅" } else { "❌" };
        format!("{} {:<14} {}", icon, stage.key, stage.message)
    }));
    if outcome.fix_result.attempted {
        if outcome.fix_result.applied {
            let label = if outcome.fix_result.created {
                "created"
            } else {
                "updated"
            };
            lines.push(format!("🛠️  workflow {} via --fix", label));
        } else {
            lines.push("🛠️  --fix requested, but workflow was already up-to-date".to_string());
        }
    }
    lines
}

pub fn official_publish_failure_action(outcome: &OfficialPublishOutcome) -> String {
    crate::publish_official::collect_issue_actions(&outcome.diagnosis.issues)
        .into_iter()
        .next()
        .unwrap_or_else(|| "ato publish --deploy --registry https://api.ato.run".to_string())
}

pub fn official_publish_issue_lines(outcome: &OfficialPublishOutcome) -> Vec<String> {
    outcome
        .diagnosis
        .issues
        .iter()
        .map(|issue| format!(" - [{}] {}", issue.stage, issue.message))
        .collect()
}

pub struct DirectPublishDryRunRequest<'a> {
    pub registry_url: &'a str,
    pub scoped_id: &'a str,
    pub version: &'a str,
    pub artifact_version: &'a str,
    pub allow_existing: bool,
    pub requires_session_token: bool,
}

pub fn run_direct_publish_dry_run_phase(
    request: &DirectPublishDryRunRequest<'_>,
) -> Result<PublishDryRunStageResult> {
    let registry =
        crate::registry::http::normalize_registry_url(request.registry_url, "--registry")?;
    let scoped = crate::install::parse_capsule_ref(request.scoped_id)?;
    let upload_endpoint = build_direct_publish_upload_endpoint(
        &registry,
        request.scoped_id,
        request.version,
        upload_file_name_for_artifact(&scoped.slug, request.artifact_version).as_deref(),
        request.allow_existing,
    )?;
    probe_registry_reachability(&registry)?;

    let auth_ready = if request.requires_session_token {
        crate::auth::current_session_token().is_some()
    } else {
        crate::registry::http::current_ato_token().is_some()
    };

    Ok(PublishDryRunStageResult {
        kind: "direct_preflight",
        diagnosis: None,
        registry: Some(registry),
        upload_endpoint: Some(upload_endpoint),
        reachable: Some(true),
        auth_ready: Some(auth_ready),
        permission_check: Some("local_prereq_only".to_string()),
    })
}

pub fn direct_publish_dry_run_is_ready(
    result: &PublishDryRunStageResult,
    requires_session_token: bool,
) -> bool {
    let reachable = result.reachable.unwrap_or(false);
    let auth_ready = result.auth_ready.unwrap_or(false);
    if requires_session_token {
        reachable && auth_ready
    } else {
        reachable
    }
}

pub fn direct_publish_dry_run_failure_message(
    result: &PublishDryRunStageResult,
    requires_session_token: bool,
) -> String {
    if !result.reachable.unwrap_or(false) {
        return "registry reachability probe failed".to_string();
    }
    if requires_session_token && !result.auth_ready.unwrap_or(false) {
        return "Personal Dock publish dry-run requires an active session token".to_string();
    }
    if !requires_session_token && !result.auth_ready.unwrap_or(false) {
        return "publish preflight completed without ATO_TOKEN; continuing with local prereq-only readiness".to_string();
    }
    "publish preflight failed".to_string()
}

fn upload_file_name_for_artifact(slug: &str, manifest_version: &str) -> Option<String> {
    let version = manifest_version.trim();
    if version.is_empty() {
        None
    } else {
        Some(format!("{}-{}.capsule", slug, version))
    }
}

fn build_direct_publish_upload_endpoint(
    registry_url: &str,
    scoped_id: &str,
    version: &str,
    file_name: Option<&str>,
    allow_existing: bool,
) -> Result<String> {
    let scoped = crate::install::parse_capsule_ref(scoped_id)?;
    let mut endpoint = format!(
        "{}/v1/local/capsules/{}/{}/{}",
        registry_url,
        urlencoding::encode(&scoped.publisher),
        urlencoding::encode(&scoped.slug),
        urlencoding::encode(version)
    );
    if let Some(file_name) = file_name.filter(|value| !value.trim().is_empty()) {
        endpoint.push_str(&format!("?file_name={}", urlencoding::encode(file_name)));
    }
    if allow_existing {
        endpoint.push_str(if endpoint.contains('?') {
            "&allow_existing=true"
        } else {
            "?allow_existing=true"
        });
    }
    Ok(endpoint)
}

fn probe_registry_reachability(registry_url: &str) -> Result<()> {
    let client = crate::registry::http::blocking_client_builder(registry_url)
        .build()
        .map_err(|err| anyhow::anyhow!("Failed to create registry preflight client: {}", err))?;
    client
        .get(registry_url)
        .send()
        .map(|_| ())
        .map_err(|err| anyhow::anyhow!("Failed to reach registry {}: {}", registry_url, err))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;

    use super::{PublishPhase, PublishPhaseRequest};
    use crate::application::ports::publish::{
        DestinationPort, DestinationSpec, PublishableArtifact, PublishedLocation,
    };

    #[derive(Debug)]
    struct StubDestination;

    #[async_trait]
    impl DestinationPort for StubDestination {
        async fn publish(
            &self,
            artifact: &PublishableArtifact,
            destination: &DestinationSpec,
        ) -> anyhow::Result<PublishedLocation> {
            Ok(PublishedLocation {
                destination: destination.clone(),
                receipt: format!("published {}", artifact.normalized_file_name),
                locator: "memory://published".to_string(),
            })
        }
    }

    #[tokio::test]
    async fn publish_phase_routes_artifact_to_destination_port() {
        let phase = PublishPhase::new(Arc::new(StubDestination));
        let request = PublishPhaseRequest {
            artifact: PublishableArtifact {
                bytes: b"capsule".to_vec(),
                scoped_id: "capsules/demo".to_string(),
                version: "0.1.0".to_string(),
                normalized_file_name: "demo-0.1.0.capsule".to_string(),
                content_hash: "blake3:demo".to_string(),
            },
            destination: DestinationSpec::RemoteRegistry {
                registry_url: "https://example.invalid".to_string(),
                scoped_id: "capsules/demo".to_string(),
                version: "0.1.0".to_string(),
            },
        };

        let published = phase.execute(&request).await.expect("publish");

        assert_eq!(published.receipt, "published demo-0.1.0.capsule");
        assert_eq!(published.locator, "memory://published");
    }
}
