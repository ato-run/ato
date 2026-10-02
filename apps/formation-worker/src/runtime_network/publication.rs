//! Finish immutable evidence publication; this module never executes a candidate.
use super::{AttemptResultReport, AttemptTicket, Client, ServeConfig, delivery::Delivery};
use anyhow::{Context, Result, ensure};
use ato_formation::{build_record::BuildRecordManifest, retained::content_ref};
use ato_runtime_attempt::{
    journal::{AttemptJournal, AttemptRecordState, AttemptState},
    plan::PlannedCandidate,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    schema: String,
    ticket_ref: String,
    workspace: String,
    provenance: crate::retained::RetentionProvenance,
    manifest: BuildRecordManifest,
    report: AttemptResultReport,
}
fn directory(config: &ServeConfig, ticket: &AttemptTicket) -> PathBuf {
    let identity = content_ref(format!("{}:{}", ticket.attempt_id, ticket.fence).as_bytes());
    config.out_dir.join("publication").join(&identity[7..])
}
pub(super) fn delivery(config: &ServeConfig, ticket: &AttemptTicket) -> Result<Delivery> {
    Delivery::open(&directory(config, ticket).join("delivery"))
}
fn save(path: &Path, bytes: &[u8]) -> Result<()> {
    if path.exists() {
        ensure!(
            std::fs::read(path)? == bytes,
            "publication_checkpoint_changed"
        );
        return Ok(());
    }
    super::proposal::reasoning::save_owner_checkpoint(path, bytes)
}
fn confirmed(config: &ServeConfig, ticket: &AttemptTicket) -> Result<()> {
    let records =
        AttemptJournal::new(config.out_dir.join("attempt-records")).records(&ticket.satisfy_id)?;
    ensure!(
        !records.iter().any(|r| r.state == AttemptState::Started),
        "publication_execution_unknown"
    );
    let original = records
        .iter()
        .find(|r| r.attempt_id == ticket.attempt_id)
        .context("publication_execution_missing")?;
    let identity = original
        .identity
        .as_ref()
        .context("publication_execution_missing")?;
    ensure!(
        original.state == AttemptState::Finished
            && original.outcome.as_deref() == Some("verified")
            && identity.contract_ref == ticket.contract_ref
            && identity.derivation_ref == ticket.derivation_ref
            && identity.runtime_id == ticket.runtime_id,
        "publication_execution_unconfirmed"
    );
    Ok(())
}
fn validate(config: &ServeConfig, ticket: &AttemptTicket, checkpoint: &Checkpoint) -> Result<()> {
    confirmed(config, ticket)?;
    ensure!(
        checkpoint.schema == "ato.runtime-publication-checkpoint/1"
            && checkpoint.ticket_ref == content_ref(&serde_jcs::to_vec(ticket)?)
            && checkpoint.provenance.contract_ref()? == ticket.contract_ref
            && checkpoint.provenance.derivation_ref()? == ticket.derivation_ref
            && checkpoint.report.outcome == "pass"
            && checkpoint.report.fence == ticket.fence
            && checkpoint.report.attestation.attempt_record == AttemptRecordState::Finished,
        "publication_checkpoint_assignment_mismatch"
    );
    ensure!(
        ato_formation::proposal::source_path(&checkpoint.workspace),
        "publication_workspace_invalid"
    );
    let source = checkpoint
        .provenance
        .derivation
        .inputs
        .iter()
        .find(|i| i.protocol == "ato.workspace@1")
        .context("publication_source_missing")?;
    checkpoint.manifest.match_assignment(
        &ticket.contract_ref,
        &checkpoint.provenance.derivation,
        &source.content_ref,
        &ticket.attempt_id,
    )?;
    Ok(())
}
fn failure(mut report: AttemptResultReport, error: Option<&anyhow::Error>) -> AttemptResultReport {
    let code = if error.is_some_and(|e| e.to_string() == "search_stored_budget_exceeded") {
        "search_stored_budget_exceeded"
    } else {
        "retained_publication_failed"
    };
    report.outcome = "fail".into();
    report.materialization_ref = None;
    report.retained_ref = None;
    // Private filesystem paths, source metadata and response bodies stay local.
    report.failure = Some(super::AttemptFailureWire {
        code: code.into(),
        stage: "publish".into(),
        message: "immutable build evidence or workspace publication could not be completed".into(),
    });
    if let Some(attempt) = &mut report.formation_attempt {
        attempt["status"] = serde_json::json!("failed");
        attempt["outcomes"]["publication"] = serde_json::json!({"state":"failed","reason":code});
        attempt["failure"] = serde_json::to_value(&report.failure).unwrap_or_default();
    }
    report
}

pub(super) fn begin(
    config: &ServeConfig,
    ticket: &AttemptTicket,
    client: &Client,
    workspace: &Path,
    planned: &PlannedCandidate,
    report: AttemptResultReport,
) -> AttemptResultReport {
    let mut report = report;
    let prepared = (|| -> Result<Checkpoint> {
        confirmed(config, ticket)?;
        let record = crate::build_record::prepare(
            workspace,
            &planned.derivation,
            &ticket.contract_ref,
            &ticket.attempt_id,
        )?
        .context("registered build evidence missing")?;
        ensure!(
            record.manifest.stored_bytes()? <= ticket.resource_budget.stored_bytes,
            "search_stored_budget_exceeded"
        );
        report.resource_usage.stored_bytes = record.manifest.stored_bytes()?;
        let directory = directory(config, ticket);
        let chunks = directory.join("chunks");
        std::fs::create_dir_all(&chunks)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
        }
        for (reference, bytes) in record.chunks {
            save(&chunks.join(&reference[7..]), &bytes)?;
        }
        let relative = std::fs::canonicalize(workspace)?
            .strip_prefix(std::fs::canonicalize(&config.work_root)?)?
            .to_str()
            .context("publication_workspace_invalid")?
            .to_owned();
        let checkpoint = Checkpoint {
            schema: "ato.runtime-publication-checkpoint/1".into(),
            ticket_ref: content_ref(&serde_jcs::to_vec(ticket)?),
            workspace: relative,
            provenance: crate::retained::RetentionProvenance::process(
                planned,
                ticket.browser_contract.as_ref(),
            ),
            manifest: record.manifest,
            report: report.clone(),
        };
        validate(config, ticket, &checkpoint)?;
        save(
            &directory.join("checkpoint.json"),
            &serde_jcs::to_vec(&checkpoint)?,
        )?;
        Ok(checkpoint)
    })();
    match prepared {
        Ok(checkpoint) => complete(config, ticket, client, &checkpoint).unwrap_or_else(|_| {
            let mut conservative = report;
            conservative.resource_usage.stored_bytes = ticket.resource_budget.stored_bytes;
            failure(conservative, None)
        }),
        Err(error) => failure(report, Some(&error)),
    }
}

pub(super) fn resume(
    config: &ServeConfig,
    ticket: &AttemptTicket,
    client: &Client,
) -> Result<Option<AttemptResultReport>> {
    let path = directory(config, ticket).join("checkpoint.json");
    if !path.exists() {
        return Ok(None);
    }
    ensure!(
        std::fs::symlink_metadata(&path)?.is_file()
            && std::fs::metadata(&path)?.len() <= 2 * 1024 * 1024,
        "publication_checkpoint_invalid"
    );
    let checkpoint: Checkpoint = serde_json::from_slice(&std::fs::read(path)?)?;
    validate(config, ticket, &checkpoint)?;
    let report = complete(config, ticket, client, &checkpoint)?;
    // The original verified computation is finished. Only its owned scratch
    // can now be removed; there was no new realization on this path.
    let _ = std::fs::remove_dir_all(config.work_root.join(&ticket.attempt_id));
    Ok(Some(report))
}

fn complete(
    config: &ServeConfig,
    ticket: &AttemptTicket,
    client: &Client,
    checkpoint: &Checkpoint,
) -> Result<AttemptResultReport> {
    validate(config, ticket, checkpoint)?;
    let delivery = delivery(config, ticket)?;
    let directory = directory(config, ticket);
    let result = directory.join("report.json");
    if result.exists() {
        return Ok(serde_json::from_slice(&std::fs::read(result)?)?);
    }
    let mut report = checkpoint.report.clone();
    // Charge the complete reserved evidence conservatively if any upload may
    // have happened, even when its reply was lost. Never refund on restart.
    report.resource_usage.stored_bytes = checkpoint.manifest.stored_bytes()?;
    let start = std::time::Instant::now();
    let publication = (|| -> Result<()> {
        let client = client.for_reporting();
        let ready = client.build_record(
            ticket,
            &checkpoint.manifest,
            &directory.join("chunks"),
            &delivery,
        )?;
        report.build_record_ref = Some(ready.reference()?);
        let workspace = config.work_root.join(&checkpoint.workspace);
        let canonical = std::fs::canonicalize(&workspace)?;
        ensure!(
            canonical.starts_with(std::fs::canonicalize(&config.work_root)?),
            "publication_workspace_invalid"
        );
        let prepared = crate::retained::prepare_process(
            &workspace,
            &checkpoint.provenance,
            &ticket.attempt_id,
            &ready,
        )?;
        let stored = ready
            .stored_bytes()?
            .checked_add(prepared.descriptor.artifact.bytes)
            .context("search_stored_budget_exceeded")?;
        ensure!(
            stored <= ticket.resource_budget.stored_bytes,
            "search_stored_budget_exceeded"
        );
        report.resource_usage.stored_bytes = stored;
        let materialization = crate::local::PreparedArtifact::Process {
            packed: prepared.bytes.clone(),
        }
        .store(&config.out_dir)?;
        ensure!(
            materialization == prepared.descriptor.materialization_ref,
            "publication_materialization_mismatch"
        );
        report.materialization_ref = Some(materialization);
        if let Some(attempt) = &mut report.formation_attempt {
            attempt["outcomes"]["publication"] = serde_json::json!({"state":"succeeded"});
        }
        report.retained_ref = Some(client.retain(ticket, &prepared, &report, &delivery)?);
        report.verifier_receipts.push(serde_json::json!({"kind":"build_record_publication",
            "record_ref":report.build_record_ref,"stored_bytes":ready.stored_bytes()?,
            "files":checkpoint.manifest.files.len(),"elapsed_ms":start.elapsed().as_millis() as u64}));
        Ok(())
    })();
    if let Err(error) = publication {
        let _ = save(
            &directory.join("publication-error.json"),
            &serde_jcs::to_vec(&serde_json::json!({
                "schema":"ato.runtime-publication-error/1", "error":crate::api::bounded_reason(&format!("{error:#}"))
            }))?,
        );
        report = failure(report, Some(&error));
    }
    save(&result, &serde_jcs::to_vec(&report)?)?;
    Ok(report)
}

impl Client {
    fn build_record(
        &self,
        ticket: &AttemptTicket,
        manifest: &BuildRecordManifest,
        chunks: &Path,
        delivery: &Delivery,
    ) -> Result<crate::build_record::ReadyBuildRecord> {
        let reference = manifest.record_ref()?;
        let retries = ticket.exploration.as_ref().map_or(3, |e| e.max_retries);
        let path = format!("/attempts/{}/build-records", ticket.attempt_id);
        let body = serde_json::json!({"fence":ticket.fence,"record_ref":reference,
            "manifest_json":String::from_utf8(manifest.canonical_bytes()?)?});
        let pending: serde_json::Value =
            delivery.evidence("build-record-reserve", retries, || {
                self.send(self.http.post(self.url(&path)).json(&body))?
                    .context("missing evidence reservation")
            })?;
        ensure!(
            pending["record_ref"] == reference
                && pending["bytes"].as_u64() == Some(manifest.stored_bytes()?),
            "build_record_reservation_mismatch"
        );
        if pending["status"] == "ready" {
            return crate::build_record::PreparedBuildRecord {
                manifest: manifest.clone(),
                chunks: Default::default(),
            }
            .acknowledge(&pending);
        }
        let mut expected = std::collections::BTreeMap::new();
        for chunk in manifest.files.iter().flat_map(|f| &f.chunks) {
            expected.insert(&chunk.content_ref, chunk.bytes);
        }
        for (chunk_ref, bytes) in expected {
            let file = chunks.join(&chunk_ref[7..]);
            ensure!(
                std::fs::symlink_metadata(&file)?.is_file()
                    && std::fs::metadata(&file)?.len() == bytes,
                "build_record_chunk_invalid"
            );
            let chunk = std::fs::read(&file)?;
            ensure!(
                content_ref(&chunk) == *chunk_ref,
                "build_record_chunk_changed"
            );
            let _: serde_json::Value =
                delivery.evidence(&format!("chunk-{}", &chunk_ref[7..]), retries, || {
                    self.send(
                        self.http
                            .put(self.url(&format!("{path}/{reference}/chunks/{chunk_ref}")))
                            .header("x-ato-attempt-fence", ticket.fence)
                            .body(chunk.clone()),
                    )?
                    .context("missing evidence upload acknowledgement")
                })?;
        }
        let ready: serde_json::Value =
            delivery.evidence("build-record-finalize", retries, || {
                self.send(
                    self.http
                        .post(self.url(&format!("{path}/{reference}/finalize")))
                        .header("x-ato-attempt-fence", ticket.fence),
                )?
                .context("missing evidence finalization")
            })?;
        crate::build_record::PreparedBuildRecord {
            manifest: manifest.clone(),
            chunks: Default::default(),
        }
        .acknowledge(&ready)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ato_runtime_attempt::journal::{AttemptLedger, StartIdentity};
    #[test]
    fn publication_requires_the_original_finished_identity_and_refuses_unknown_history() {
        let scratch = tempfile::tempdir_in(".tmp").unwrap();
        let config = ServeConfig {
            api: "http://127.0.0.1:1".into(),
            token: "fixture".into(),
            work_root: scratch.path().join("work"),
            out_dir: scratch.path().join("out"),
            shim: PathBuf::new(),
            browser_verifier: None,
            poll: std::time::Duration::ZERO,
            max_tickets: None,
            exploration: None,
        };
        let mut ticket: AttemptTicket = serde_json::from_str(include_str!(
            "../../tests/fixtures/runtime-network-v0/attempt-ticket-source.json"
        ))
        .unwrap();
        let journal = AttemptJournal::new(config.out_dir.join("attempt-records"));
        assert!(confirmed(&config, &ticket).is_err());
        let mut permit = journal
            .acquire(&ticket.satisfy_id, &ticket.attempt_id)
            .unwrap();
        let started = permit
            .start(StartIdentity {
                contract_ref: ticket.contract_ref.clone(),
                derivation_ref: ticket.derivation_ref.clone(),
                runtime_id: ticket.runtime_id.clone(),
                effects: "isolated".into(),
                network: "denied".into(),
                authorization: "exploration".into(),
            })
            .unwrap();
        assert!(confirmed(&config, &ticket).is_err());
        started.finish("verified").unwrap();
        drop(permit);
        assert!(confirmed(&config, &ticket).is_ok());
        let original = ticket.derivation_ref.clone();
        ticket.derivation_ref = content_ref(b"another D");
        assert!(confirmed(&config, &ticket).is_err());
        ticket.derivation_ref = original;
        let mut sibling = journal
            .acquire(&ticket.satisfy_id, "unfinished-sibling")
            .unwrap();
        let unknown = sibling
            .start(StartIdentity {
                contract_ref: ticket.contract_ref.clone(),
                derivation_ref: ticket.derivation_ref.clone(),
                runtime_id: ticket.runtime_id.clone(),
                effects: "isolated".into(),
                network: "denied".into(),
                authorization: "exploration".into(),
            })
            .unwrap();
        drop(unknown);
        assert!(confirmed(&config, &ticket).is_err());
    }
}
