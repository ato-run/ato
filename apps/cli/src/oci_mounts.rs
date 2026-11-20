//! Pre-run diagnosis of the mount paths an OCI image declares.
//!
//! An image can declare `VOLUME` paths in its configuration. Docker creates an
//! anonymous writable volume for each one unless the container has a mount at
//! exactly that guest path. This profile allows one filesystem state slot, the
//! workspace mount and `/tmp`, so a manifest cannot assign every path that an
//! image such as GitLab CE (three) or ONLYOFFICE Docs (four) declares; a Run
//! refuses to start rather than create anonymous volumes.
//!
//! The refusal is only useful if the unassigned paths are named before the
//! attempt. `ato pack`, `ato hub publish` and `ato run` print them from the
//! image configuration. When that configuration cannot be read — no Docker CLI,
//! an unreachable daemon, or an image that is not present locally — the state is
//! reported as `checked=no` with the reason instead of being silently omitted.

use std::collections::BTreeMap;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ato_formation::authoring::BoundStep;
use ato_portable_application::{
    OCI_IMAGE_RUNTIME, OCI_WORKSPACE_MOUNT_RUNTIME, ValidatedPortableApplication,
};

/// A daemon that hangs must not hang `ato pack`.
const INSPECT_TIMEOUT: Duration = Duration::from_secs(10);

/// One OCI serving step as the diagnosis needs it: the image it runs and the
/// guest paths the manifest already mounts.
struct StepImage {
    derivation_ref: String,
    service: String,
    image: String,
    covered: Vec<String>,
}

/// One diagnosis line per OCI serving step, in the `field=value` vocabulary
/// `capability` lines already use.
pub(crate) fn lines(application: &ValidatedPortableApplication) -> Vec<String> {
    lines_for(&step_images(application), inspect_volumes)
}

/// The OCI serving steps of every Adapter this route declares: the single
/// container route names its image on the route, a service group on each step.
fn step_images(application: &ValidatedPortableApplication) -> Vec<StepImage> {
    let derivation_ref = application.derivation_ref.to_string();
    let mut images = Vec::new();
    for step in &application.derivation.steps {
        let Some(image) = step.runtimes.get(OCI_IMAGE_RUNTIME) else {
            continue;
        };
        images.push(StepImage {
            derivation_ref: derivation_ref.clone(),
            service: step.id.clone(),
            image: image.clone(),
            covered: covered_targets(application, step),
        });
    }
    images
}

/// Whether these lines report a problem a publisher should see.
pub(crate) fn needs_attention(lines: &[String]) -> bool {
    lines.iter().any(|line| {
        line.contains("checked=no")
            || line
                .split_once("unassigned=")
                .is_some_and(|(_, paths)| paths.trim() != "none" && !paths.trim().is_empty())
    })
}

fn lines_for(
    images: &[StepImage],
    inspect: impl Fn(&str) -> Result<Vec<String>, String>,
) -> Vec<String> {
    let mut inspected: BTreeMap<&str, Result<Vec<String>, String>> = BTreeMap::new();
    let mut lines = Vec::new();
    let mut unassigned_any = false;
    let mut unchecked_any = false;
    for step in images {
        let checked = inspected
            .entry(step.image.as_str())
            .or_insert_with(|| inspect(&step.image));
        let head = format!(
            "oci-mount derivation_ref={} service={} image={}",
            step.derivation_ref, step.service, step.image
        );
        match checked {
            Ok(volumes) => {
                let covered = &step.covered;
                let unassigned = volumes
                    .iter()
                    .map(|path| normalize(path))
                    .filter(|path| !covered.iter().any(|target| target == path))
                    .collect::<Vec<_>>();
                if unassigned.is_empty() {
                    lines.push(format!("{head} checked=yes unassigned=none"));
                } else {
                    unassigned_any = true;
                    lines.push(format!(
                        "{head} checked=yes unassigned={}",
                        unassigned.join(",")
                    ));
                }
                lines.push(format!(
                    "{head} declared_targets={}",
                    if covered.is_empty() {
                        "none".to_owned()
                    } else {
                        covered.join(",")
                    }
                ));
            }
            Err(reason) => {
                unchecked_any = true;
                lines.push(format!("{head} checked=no reason={reason}"));
            }
        }
    }
    if unassigned_any {
        lines.push(
            "oci-mount next=a Run refuses an image VOLUME with no mount at exactly its path. \
             This profile declares one filesystem state slot, the workspace mount and /tmp; \
             pick an image without extra VOLUMEs or wait for a profile that can assign them."
                .to_owned(),
        );
    }
    if unchecked_any {
        lines.push(
            "oci-mount next=the image configuration was not checked. Start Docker \
             (or pull the image) and rerun this command to list its VOLUME paths."
                .to_owned(),
        );
    }
    lines
}

/// The guest paths a declared mount already covers at exactly that path.
fn covered_targets(application: &ValidatedPortableApplication, step: &BoundStep) -> Vec<String> {
    let mut targets = vec![
        normalize(
            step.runtimes
                .get(OCI_WORKSPACE_MOUNT_RUNTIME)
                .map(String::as_str)
                .unwrap_or("/app"),
        ),
        "/tmp".to_owned(),
    ];
    for id in &step.state {
        if let Some(state) = application
            .derivation
            .state
            .iter()
            .find(|state| &state.id == id)
        {
            targets.push(normalize(&state.mount));
        }
    }
    targets
}

fn normalize(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// The image's `Config.Volumes` keys, read through the Docker CLI.
fn inspect_volumes(image: &str) -> Result<Vec<String>, String> {
    let output = run_bounded(
        "docker",
        &[
            "image",
            "inspect",
            "--format",
            "{{json .Config.Volumes}}",
            image,
        ],
        INSPECT_TIMEOUT,
    )?;
    let text = output.trim();
    if text.is_empty() || text == "null" {
        return Ok(Vec::new());
    }
    let volumes: BTreeMap<String, serde_json::Value> = serde_json::from_str(text)
        .map_err(|_| "docker image inspect returned unexpected output")?;
    Ok(volumes.into_keys().collect())
}

/// Run a command and stop waiting after `timeout`.
///
/// `std` has no wait-with-timeout, so the child is polled and killed on
/// expiry: a wedged daemon must not wedge the diagnosis.
fn run_bounded(program: &str, arguments: &[&str], timeout: Duration) -> Result<String, String> {
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("the docker CLI did not run: {error}"))?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = child
                    .wait_with_output()
                    .map_err(|error| format!("could not read docker output: {error}"))?;
                if status.success() {
                    return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
                }
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stderr = stderr.trim();
                let lowered = stderr.to_ascii_lowercase();
                return Err(
                    if lowered.contains("no such image") || lowered.contains("no such object") {
                        "the image is not present locally".to_owned()
                    } else if stderr.is_empty() {
                        format!("docker image inspect exited with {status} and no message")
                    } else {
                        format!("docker image inspect failed: {}", bounded(stderr))
                    },
                );
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "docker image inspect did not answer within {}s",
                    timeout.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(error) => return Err(format!("could not wait for docker: {error}")),
        }
    }
}

fn bounded(text: &str) -> String {
    text.chars().take(300).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(service: &str, covered: &[&str]) -> StepImage {
        StepImage {
            derivation_ref: "sha256:route".to_owned(),
            service: service.to_owned(),
            image: "example/app@sha256:11".to_owned(),
            covered: covered.iter().map(|path| normalize(path)).collect(),
        }
    }

    #[test]
    fn a_volume_without_one_exact_mount_is_listed_as_unassigned() {
        // A GitLab CE-shaped image: three VOLUMEs, one declared state slot.
        let images = [step("gitlab", &["/etc/gitlab", "/app", "/tmp"])];
        let lines = lines_for(&images, |_| {
            Ok(vec![
                "/etc/gitlab".to_owned(),
                "/var/opt/gitlab".to_owned(),
                "/var/log/gitlab".to_owned(),
            ])
        });
        assert!(
            lines[0].contains("checked=yes unassigned=/var/opt/gitlab,/var/log/gitlab"),
            "{lines:?}"
        );
        assert!(needs_attention(&lines));
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("oci-mount next=a Run refuses")),
            "{lines:?}"
        );
    }

    #[test]
    fn an_image_whose_volumes_are_all_mounted_needs_no_attention() {
        let images = [step("app", &["/data/", "/app", "/tmp"])];
        let lines = lines_for(&images, |_| Ok(vec!["/data".to_owned()]));
        assert!(
            lines[0].contains("checked=yes unassigned=none"),
            "{lines:?}"
        );
        assert!(!needs_attention(&lines));
    }

    #[test]
    fn an_unreadable_image_configuration_is_reported_as_unchecked() {
        let images = [step("gitlab", &["/app", "/tmp"])];
        let lines = lines_for(&images, |_| {
            Err("the image is not present locally".to_owned())
        });
        assert!(
            lines[0].contains("checked=no reason=the image is not present locally"),
            "{lines:?}"
        );
        assert!(needs_attention(&lines));
        assert!(
            lines.iter().any(|line| line.contains("was not checked")),
            "{lines:?}"
        );
    }

    #[test]
    fn a_service_group_inspects_each_image_once() {
        let images = [step("web", &["/app"]), step("db", &["/app"])];
        let inspections = std::cell::Cell::new(0);
        let lines = lines_for(&images, |_| {
            inspections.set(inspections.get() + 1);
            Ok(Vec::new())
        });
        assert_eq!(inspections.get(), 1);
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.contains("checked=yes"))
                .count(),
            2
        );
    }
}
