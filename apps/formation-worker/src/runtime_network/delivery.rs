//! Runtime delivery journal. Claim/result response loss never re-enters execution.
use super::{AttemptResultReport, AttemptTicket, Client};
use anyhow::{Context, Result, ensure};
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
        mut send: impl FnMut() -> Result<T>,
    ) -> Result<T> {
        let result = dir.join(format!("{kind}.response.json"));
        if result.exists() {
            return Ok(serde_json::from_slice(&std::fs::read(result)?)?);
        }
        for retry in 0..=retries {
            let dispatched = dir.join(format!("{kind}.dispatch{retry}.json"));
            if dispatched.exists() {
                continue;
            }
            self.save(&dispatched,&json!({"operation":kind,"retry":retry,"at_ms":SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis()}))?;
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
                    if retry < retries {
                        std::thread::sleep(Duration::from_millis(1000_u64 << retry.min(5)));
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
                self.dispatch(&dir, "claim", 3, || client.claim_operation(&id))?;
            if ticket.is_none() {
                self.close(&dir)?;
            }
            return Ok(ticket);
        }
    }
    pub fn report(
        &self,
        client: &Client,
        ticket: &AttemptTicket,
        report: &AttemptResultReport,
    ) -> Result<()> {
        let (_, dir) = self.operation()?;
        self.save(&dir.join("result.json"), report)?;
        self.dispatch(&dir, "result", 3, || {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lost_runtime_dispatch_stays_the_same_operation_after_restart() {
        let root = tempfile::tempdir_in(".tmp").unwrap();
        let mut calls = 0;
        let delivery = Delivery::open(root.path()).unwrap();
        let (id, directory) = delivery.operation().unwrap();
        assert!(
            delivery
                .dispatch::<Value>(&directory, "claim", 3, || {
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
                .dispatch::<Value>(&directory, "claim", 3, || panic!(
                    "retry count must not reset"
                ))
                .is_err()
        );
        let result = resumed
            .dispatch(&directory, "result", 3, || {
                Ok(json!({"attempt_id":"same","accepted":true}))
            })
            .unwrap();
        assert_eq!(
            resumed
                .dispatch::<Value>(&directory, "result", 3, || panic!(
                    "saved result must not be resent"
                ))
                .unwrap(),
            result
        );
    }
}
