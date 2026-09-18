//! Non-secret ownership labels, and finding what a Runner slot owns.
//!
//! Every container and network a Runner creates carries who created it: the
//! Runner, its slot, the lease and Run, and the process incarnation. After a
//! Runner restart these labels — not process memory — are how it finds what
//! it left running. A resource without the exact labels is never touched:
//! not another slot's, not another Runner's, not a container Ato did not make.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};

use crate::stop::{
    DOCKER_CALL_TIMEOUT, StopBudget, StopOutcome, docker_output, remove_stopped_container,
    stop_container,
};

pub const LABEL_MANAGED: &str = "run.ato.dev/managed";
pub const LABEL_RUNNER_ID: &str = "run.ato.dev/runner-id";
pub const LABEL_SLOT_ID: &str = "run.ato.dev/slot-id";
pub const LABEL_LEASE_ID: &str = "run.ato.dev/lease-id";
pub const LABEL_RUN_ID: &str = "run.ato.dev/run-id";
pub const LABEL_INCARNATION: &str = "run.ato.dev/incarnation";
pub const LABEL_SERVICE: &str = "run.ato.dev/service";

/// Who created a container or network. Rendered as Docker labels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciOwner {
    pub runner_id: String,
    pub slot_id: String,
    pub lease_id: String,
    pub run_id: String,
    pub incarnation: String,
}

/// Label values are identifiers; restricting them keeps `docker ps` output
/// (comma-separated `k=v`) unambiguous and keeps anything secret-shaped out.
pub fn is_label_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
}

impl OciOwner {
    pub fn labels(&self, service: Option<&str>) -> Result<BTreeMap<String, String>> {
        let mut labels = BTreeMap::from([
            (LABEL_MANAGED.to_owned(), "true".to_owned()),
            (LABEL_RUNNER_ID.to_owned(), self.runner_id.clone()),
            (LABEL_SLOT_ID.to_owned(), self.slot_id.clone()),
            (LABEL_LEASE_ID.to_owned(), self.lease_id.clone()),
            (LABEL_RUN_ID.to_owned(), self.run_id.clone()),
            (LABEL_INCARNATION.to_owned(), self.incarnation.clone()),
        ]);
        if let Some(service) = service {
            labels.insert(LABEL_SERVICE.to_owned(), service.to_owned());
        }
        for (key, value) in &labels {
            ensure!(
                is_label_value(value),
                "ownership label `{key}` is not an identifier"
            );
        }
        Ok(labels)
    }
}

/// A container or network found by its ownership labels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedResource {
    pub id: String,
    pub name: String,
    pub labels: BTreeMap<String, String>,
}

impl OwnedResource {
    pub fn lease_id(&self) -> Option<&str> {
        self.labels.get(LABEL_LEASE_ID).map(String::as_str)
    }
    pub fn run_id(&self) -> Option<&str> {
        self.labels.get(LABEL_RUN_ID).map(String::as_str)
    }
    pub fn service(&self) -> Option<&str> {
        self.labels.get(LABEL_SERVICE).map(String::as_str)
    }
}

/// Everything one Runner slot owns, as Docker reports it now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OwnedResources {
    pub containers: Vec<OwnedResource>,
    pub networks: Vec<OwnedResource>,
}

fn parse_labels(raw: &str) -> BTreeMap<String, String> {
    raw.split(',')
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

/// True only when the resource carries exactly this slot's ownership. Docker's
/// label filter already narrowed the list; this re-check is what makes the
/// "never touch someone else's" rule hold even if a filter were ignored.
fn owned_by(labels: &BTreeMap<String, String>, runner_id: &str, slot_id: &str) -> bool {
    labels.get(LABEL_MANAGED).map(String::as_str) == Some("true")
        && labels.get(LABEL_RUNNER_ID).map(String::as_str) == Some(runner_id)
        && labels.get(LABEL_SLOT_ID).map(String::as_str) == Some(slot_id)
}

fn filters(runner_id: &str, slot_id: &str) -> Vec<String> {
    vec![
        "--filter".to_owned(),
        format!("label={LABEL_MANAGED}=true"),
        "--filter".to_owned(),
        format!("label={LABEL_RUNNER_ID}={runner_id}"),
        "--filter".to_owned(),
        format!("label={LABEL_SLOT_ID}={slot_id}"),
    ]
}

fn list(
    docker: &Path,
    mut arguments: Vec<String>,
    name_field: &str,
    runner_id: &str,
    slot_id: &str,
) -> Result<Vec<OwnedResource>> {
    arguments.extend(filters(runner_id, slot_id));
    arguments.extend([
        "--format".to_owned(),
        format!("{{{{.ID}}}}|{{{{{name_field}}}}}|{{{{.Labels}}}}"),
    ]);
    let output = docker_output(
        docker,
        arguments.iter().map(String::as_str),
        DOCKER_CALL_TIMEOUT,
    )?;
    if !output.status.success() {
        bail!(
            "list Runner-owned OCI resources failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let text = String::from_utf8(output.stdout).context("Docker returned non-UTF-8")?;
    Ok(text
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '|');
            let id = parts.next()?.trim().to_owned();
            let name = parts.next()?.trim().to_owned();
            let labels = parse_labels(parts.next().unwrap_or(""));
            (!id.is_empty() && owned_by(&labels, runner_id, slot_id)).then_some(OwnedResource {
                id,
                name,
                labels,
            })
        })
        .collect())
}

/// Finds every container and network owned by one Runner slot.
pub struct OwnedResourceScanner {
    docker: PathBuf,
    runner_id: String,
    slot_id: String,
}

impl OwnedResourceScanner {
    pub fn new(runner_id: &str, slot_id: &str) -> Result<Self> {
        ensure!(
            is_label_value(runner_id) && is_label_value(slot_id),
            "Runner and slot ids must be label identifiers"
        );
        let docker = crate::find_on_path("docker")
            .context("Docker CLI is not installed or is not on PATH")?;
        Ok(Self::with_docker(docker, runner_id, slot_id))
    }

    pub fn with_docker(docker: PathBuf, runner_id: &str, slot_id: &str) -> Self {
        Self {
            docker,
            runner_id: runner_id.to_owned(),
            slot_id: slot_id.to_owned(),
        }
    }

    /// Current containers (running or not) and networks of this slot.
    pub fn scan(&self) -> Result<OwnedResources> {
        Ok(OwnedResources {
            containers: list(
                &self.docker,
                vec!["ps".to_owned(), "--all".to_owned(), "--no-trunc".to_owned()],
                ".Names",
                &self.runner_id,
                &self.slot_id,
            )?,
            networks: list(
                &self.docker,
                vec![
                    "network".to_owned(),
                    "ls".to_owned(),
                    "--no-trunc".to_owned(),
                ],
                ".Name",
                &self.runner_id,
                &self.slot_id,
            )?,
        })
    }

    /// Stop one owned container and, once confirmed, remove it.
    pub fn stop_container(&self, container: &OwnedResource, budget: StopBudget) -> StopOutcome {
        if !owned_by(&container.labels, &self.runner_id, &self.slot_id) {
            return StopOutcome::Unconfirmed {
                reason: "refusing to stop a container this slot does not own".to_owned(),
            };
        }
        let outcome = stop_container(&self.docker, &container.id, budget);
        if outcome.is_confirmed() {
            // A stopped container that could not be removed writes nothing;
            // it is left for the next scan rather than force-removed.
            let _ = remove_stopped_container(&self.docker, &container.id);
        }
        outcome
    }

    /// Remove an owned network. Fails while any container is attached.
    pub fn remove_network(&self, network: &OwnedResource) -> Result<()> {
        ensure!(
            owned_by(&network.labels, &self.runner_id, &self.slot_id),
            "refusing to remove a network this slot does not own"
        );
        crate::remove_network(&self.docker, &network.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner() -> OciOwner {
        OciOwner {
            runner_id: "01KX0SWDPP2GA41NEXQXNDCC0D".to_owned(),
            slot_id: "staging-fc-1".to_owned(),
            lease_id: "01M2T88K4ZG6C3NSN46C7PCEYB".to_owned(),
            run_id: "run_01M2T88K4ZKS9FDJ33BWMKJMAM".to_owned(),
            incarnation: "01M2V0INCARNATION000000000".to_owned(),
        }
    }

    #[test]
    fn labels_carry_ownership_and_refuse_non_identifiers() {
        let labels = owner().labels(Some("backend")).unwrap();
        assert_eq!(labels[LABEL_MANAGED], "true");
        assert_eq!(labels[LABEL_SERVICE], "backend");
        let mut bad = owner();
        bad.run_id = "run,with=comma".to_owned();
        assert!(bad.labels(None).is_err());
    }

    #[test]
    fn only_this_slots_resources_count_as_owned() {
        let labels = owner().labels(None).unwrap();
        assert!(owned_by(
            &labels,
            "01KX0SWDPP2GA41NEXQXNDCC0D",
            "staging-fc-1"
        ));
        assert!(!owned_by(
            &labels,
            "01KX0SWDPP2GA41NEXQXNDCC0D",
            "staging-fc-2"
        ));
        assert!(!owned_by(&labels, "another-runner", "staging-fc-1"));
        let mut unmanaged = labels.clone();
        unmanaged.remove(LABEL_MANAGED);
        assert!(!owned_by(
            &unmanaged,
            "01KX0SWDPP2GA41NEXQXNDCC0D",
            "staging-fc-1"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_scan_drops_rows_whose_labels_do_not_match_even_if_docker_returned_them() {
        // A stand-in Docker that ignores filters and returns everything.
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("docker");
        std::fs::write(
            &fake,
            "#!/bin/sh\n\
             echo 'c1|ours|run.ato.dev/managed=true,run.ato.dev/runner-id=r1,run.ato.dev/slot-id=s1,run.ato.dev/lease-id=l1'\n\
             echo 'c2|other-slot|run.ato.dev/managed=true,run.ato.dev/runner-id=r1,run.ato.dev/slot-id=s2'\n\
             echo 'c3|swagger|com.example=1'\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let scanner = OwnedResourceScanner::with_docker(fake, "r1", "s1");
        let found = scanner.scan().unwrap();
        assert_eq!(found.containers.len(), 1);
        assert_eq!(found.containers[0].lease_id(), Some("l1"));
        let foreign = OwnedResource {
            id: "c3".to_owned(),
            name: "swagger".to_owned(),
            labels: BTreeMap::new(),
        };
        assert!(
            !scanner
                .stop_container(&foreign, StopBudget::DEFAULT)
                .is_confirmed()
        );
        assert!(scanner.remove_network(&foreign).is_err());
    }
}
