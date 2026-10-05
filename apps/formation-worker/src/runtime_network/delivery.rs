//! Runtime delivery journal. Claim/result response loss never re-enters execution.
use super::{AttemptResultReport, AttemptTicket, Client};
use anyhow::{Context, Result, ensure};
use ato_runtime_attempt::control::{AttemptPhase, ExecutionControl};
use fs2::FileExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(super) struct Delivery {
    root: PathBuf,
    _lock: std::fs::File,
}
impl Delivery {
    pub fn open(root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))?;
        }
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join("worker.lock"))?;
        lock.try_lock_exclusive()
            .context("another worker owns this delivery journal")?;
        Ok(Self {
            root: root.into(),
            _lock: lock,
        })
    }
    fn save(&self, path: &Path, value: &impl serde::Serialize) -> Result<()> {
        let bytes = serde_jcs::to_vec(value)?;
        if path.exists() {
            ensure!(std::fs::read(path)? == bytes, "runtime delivery changed");
            return Ok(());
        }
        super::proposal::reasoning::save_owner_checkpoint(path, &bytes)
    }
    fn operation(&self) -> Result<(String, PathBuf)> {
        let pointer = self.root.join("active.json");
        let id = if pointer.exists() {
            serde_json::from_slice::<Value>(&std::fs::read(&pointer)?)?["id"]
                .as_str()
                .context("delivery operation id")?
                .to_owned()
        } else {
            let random = tempfile::NamedTempFile::new_in(&self.root)?;
            let id = format!(
                "{:x}",
                Sha256::digest(
                    format!(
                        "{}:{}:{:?}",
                        random.path().display(),
                        std::process::id(),
                        SystemTime::now()
                    )
                    .as_bytes()
                )
            );
            self.save(&pointer, &json!({"id":id}))?;
            id
        };
        let directory = self.root.join(&id);
        std::fs::create_dir_all(&directory)?;
        Ok((id, directory))
    }
    fn close(&self, dir: &Path) -> Result<()> {
        self.save(&dir.join("closed.json"), &json!({"closed":true}))?;
        std::fs::remove_file(self.root.join("active.json"))?;
        Ok(())
    }
    fn dispatch<T: serde::Serialize + serde::de::DeserializeOwned>(
        &self,
        dir: &Path,
        kind: &str,
        retries: u32,
        control: Option<&ExecutionControl>,
        mut send: impl FnMut() -> Result<T>,
    ) -> Result<T> {
        ensure!(retries <= 16, "invalid frozen runtime retry limit");
        // The operation owns its retry limit, including after a worker restart.
        self.save(
            &dir.join(format!("{kind}.policy.json")),
            &json!({"max_retries":retries}),
        )?;
        if let Some(control) = control {
            self.save(
                &dir.join(format!("{kind}.deadline.json")),
                &json!({"deadline_ms":control.deadline_ms()}),
            )?;
        }
        let result = dir.join(format!("{kind}.response.json"));
        if result.exists() {
            return Ok(serde_json::from_slice(&std::fs::read(result)?)?);
        }
        for retry in 0..=retries {
            let dispatched = dir.join(format!("{kind}.dispatch{retry}.json"));
            if dispatched.exists() {
                continue;
            }
            if let Some(control) = control {
                control.remaining(AttemptPhase::Source)?;
            }
            self.save(&dispatched,&json!({"operation":kind,"retry":retry,"at_ms":SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis()}))?;
            if let Some(control) = control {
                control.remaining(AttemptPhase::Source)?;
            }
            match send() {
                Ok(reply) => {
                    self.save(&result, &reply)?;
                    return Ok(reply);
                }
                Err(error) => {
                    self.save(
                        &dir.join(format!("{kind}.error{retry}.json")),
                        &json!({"error":super::super::api::bounded_reason(&format!("{error:#}"))}),
                    )?;
                    if error.to_string().starts_with("coordinator answered 4") {
                        return Err(error);
                    }
                    if retry < retries {
                        let delay = Duration::from_millis(1000_u64 << retry.min(5));
                        let _backoff = control
                            .map(|c| c.phase(AttemptPhase::Backoff))
                            .transpose()?;
                        if let Some(control) = control {
                            ensure!(
                                control.remaining(AttemptPhase::Backoff)? > delay,
                                "runtime retry backoff exceeds the frozen deadline"
                            );
                        }
                        std::thread::sleep(delay);
                    }
                }
            }
        }
        anyhow::bail!("runtime {kind} retry limit; operation and attempt preserved")
    }
    pub fn claim(&self, client: &Client) -> Result<Option<AttemptTicket>> {
        loop {
            let (id, dir) = self.operation()?;
            if dir.join("closed.json").exists() {
                std::fs::remove_file(self.root.join("active.json"))?;
                continue;
            }
            let ticket: Option<AttemptTicket> =
                self.dispatch(&dir, "claim", 3, None, || client.claim_operation(&id))?;
            if ticket.is_none() {
                self.close(&dir)?;
            }
            return Ok(ticket);
        }
    }
    /// Grant values live only in memory. Durable markers contain delivery
    /// identity/counts, never the reply or redeemed bytes.
    pub fn binding_poll<T>(
        &self,
        retries: u32,
        control: &ExecutionControl,
        mut send: impl FnMut() -> Result<T>,
    ) -> Result<T> {
        let (_, dir) = self.operation()?;
        let mut poll = 0u32;
        while dir.join(format!("binding-{poll}.response.json")).exists() {
            poll = poll.checked_add(1).context("binding poll bound")?;
        }
        let mut reply = None;
        let _: Value = self.dispatch(
            &dir,
            &format!("binding-{poll}"),
            retries,
            Some(control),
            || {
                reply = Some(send()?);
                Ok(json!({"delivered":true}))
            },
        )?;
        reply.context("binding reply unavailable in this process")
    }
    pub fn report(
        &self,
        client: &Client,
        ticket: &AttemptTicket,
        report: &AttemptResultReport,
    ) -> Result<()> {
        let (_, dir) = self.operation()?;
        self.save(&dir.join("result.json"), report)?;
        let retries = ticket
            .exploration
            .as_ref()
            .map_or(3, |exploration| exploration.max_retries);
        self.dispatch(&dir, "result", retries, None, || {
            client.report(&ticket.attempt_id, report)?;
            Ok(json!({"accepted":true}))
        })?;
        self.close(&dir)
    }
    pub fn saved_report(&self) -> Result<Option<AttemptResultReport>> {
        let (_, dir) = self.operation()?;
        let path = dir.join("result.json");
        if path.exists() {
            Ok(Some(serde_json::from_slice(&std::fs::read(path)?)?))
        } else {
            Ok(None)
        }
    }

    /// Acquisition retries have their own durable ledger. A restart cannot
    /// download again with a fresh limit, or replace a completed input.
    pub fn input(
        &self,
        client: &Client,
        ticket: &AttemptTicket,
        work_root: &Path,
    ) -> Result<std::fs::File> {
        let (_, dir) = self.operation()?;
        let control = ticket
            .exploration
            .as_ref()
            .and_then(|e| e.deadline_ms)
            .map(ExecutionControl::new);
        let retries = ticket.exploration.as_ref().map_or(3, |e| e.max_retries);
        let input = dir.join("input.bin");
        // Atomic promotion proves the full download finished, even if this
        // process died before saving its acknowledgement. The common source
        // validator still authenticates these bytes against the ticket.
        if input.exists() {
            return Ok(std::fs::File::open(input)?);
        }
        let _: Value = self.dispatch(&dir, "input", retries, control.as_ref(), || {
            let mut downloaded = client.download_input(
                &ticket.attempt_id,
                ticket.resource_budget.transfer_bytes,
                ticket.fence,
                work_root,
                match ticket.input {
                    super::AttemptInput::Source { .. } => "source",
                    super::AttemptInput::Retained { .. } => "retained-content",
                },
            )?;
            let mut owned = tempfile::NamedTempFile::new_in(&dir)?;
            std::io::copy(&mut downloaded, &mut owned)?;
            owned.as_file().sync_all()?;
            owned.persist_noclobber(&input)?;
            Ok(json!({"acquired":true}))
        })?;
        Ok(std::fs::File::open(input)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grant_delivery_journal_never_contains_values_or_resets_lost_retry() {
        let root = tempfile::tempdir_in(".tmp").unwrap();
        let delivery = Delivery::open(root.path()).unwrap();
        let control = ExecutionControl::new(ato_runtime_attempt::control::now_ms() + 10000);
        let private = delivery
            .binding_poll(0, &control, || {
                Ok("synthetic-private-grant-value".to_string())
            })
            .unwrap();
        assert_eq!(private, "synthetic-private-grant-value");
        let (_, dir) = delivery.operation().unwrap();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            assert!(
                !String::from_utf8_lossy(&std::fs::read(entry.path()).unwrap()).contains(&private)
            );
        }
        assert!(
            delivery
                .binding_poll::<String>(0, &control, || anyhow::bail!("response lost"))
                .is_err()
        );
        drop(delivery);
        let resumed = Delivery::open(root.path()).unwrap();
        assert!(
            resumed
                .binding_poll::<String>(0, &control, || panic!(
                    "the lost retry must remain consumed"
                ))
                .is_err()
        );
    }
    #[test]
    fn lost_runtime_dispatch_stays_the_same_operation_after_restart() {
        let root = tempfile::tempdir_in(".tmp").unwrap();
        let mut calls = 0;
        let delivery = Delivery::open(root.path()).unwrap();
        let (id, directory) = delivery.operation().unwrap();
        assert!(
            delivery
                .dispatch::<Value>(&directory, "claim", 3, None, || {
                    calls += 1;
                    anyhow::bail!("response lost")
                })
                .is_err()
        );
        assert_eq!(calls, 4);
        drop(delivery);
        let resumed = Delivery::open(root.path()).unwrap();
        assert_eq!(resumed.operation().unwrap().0, id);
        assert!(
            resumed
                .dispatch::<Value>(&directory, "claim", 3, None, || panic!(
                    "retry count must not reset"
                ))
                .is_err()
        );
        let result = resumed
            .dispatch(&directory, "result", 3, None, || {
                Ok(json!({"attempt_id":"same","accepted":true}))
            })
            .unwrap();
        assert_eq!(
            resumed
                .dispatch::<Value>(&directory, "result", 3, None, || panic!(
                    "saved result must not be resent"
                ))
                .unwrap(),
            result
        );
    }
    #[test]
    fn custom_zero_retries_stays_frozen_across_restart() {
        let root = tempfile::tempdir_in(".tmp").unwrap();
        let delivery = Delivery::open(root.path()).unwrap();
        let (_, dir) = delivery.operation().unwrap();
        let mut calls = 0;
        assert!(
            delivery
                .dispatch::<Value>(&dir, "input", 0, None, || {
                    calls += 1;
                    anyhow::bail!("lost")
                })
                .is_err()
        );
        assert_eq!(calls, 1);
        drop(delivery);
        let resumed = Delivery::open(root.path()).unwrap();
        assert!(
            resumed
                .dispatch::<Value>(&dir, "input", 0, None, || panic!("no reset"))
                .is_err()
        );
        assert!(
            resumed
                .dispatch::<Value>(&dir, "input", 3, None, || panic!("no policy replacement"))
                .is_err()
        );
    }
    #[test]
    fn frozen_deadline_prevents_dispatch_and_retry_backoff_but_allows_delivery() {
        let root = tempfile::tempdir_in(".tmp").unwrap();
        let delivery = Delivery::open(root.path()).unwrap();
        let (_, dir) = delivery.operation().unwrap();
        let expired = ExecutionControl::new(0);
        assert!(
            delivery
                .dispatch::<Value>(&dir, "expired", 3, Some(&expired), || panic!(
                    "expired dispatch"
                ))
                .is_err()
        );
        assert!(!dir.join("expired.dispatch0.json").exists());
        let short = ExecutionControl::new(ato_runtime_attempt::control::now_ms() + 100);
        let mut calls = 0;
        assert!(
            delivery
                .dispatch::<Value>(&dir, "short", 1, Some(&short), || {
                    calls += 1;
                    anyhow::bail!("lost")
                })
                .is_err()
        );
        assert!(
            calls <= 1,
            "expiry during durable reservation may also prevent the initial dispatch"
        );
        assert!(!dir.join("short.dispatch1.json").exists());
        assert!(
            delivery
                .dispatch(&dir, "result", 0, None, || Ok(json!({"accepted":true})))
                .is_ok()
        );
    }
}
