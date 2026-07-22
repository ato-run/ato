//! ato#1002 Snapshot Serving v1: builder-side artifact packaging + upload to an
//! S3-compatible object store (Cloudflare R2).
//!
//! Transport packaging is ONE file per job: `artifact.tar.gz` containing exactly
//! `manifest.json`, optional `snapshot-manifest-v1.json` plus
//! `artifact-envelope-v1.json`, and `cas/` at the archive root, uploaded to
//! `<endpoint>/<bucket>/<job_id>/<artifact_manifest_hash>/artifact.tar.gz` —
//! the object key carries the immutable content identity, so a re-run of a
//! job id can never silently overwrite different bytes — via shell-out `curl
//! --aws-sigv4` (curl signs SigV4 itself — no SDK dependency on a dev/ops
//! daemon). The upload runs BEFORE the sealed ack: an upload failure becomes a
//! failed ack at stage `artifact_upload`, never a sealed registry row without
//! fetchable bytes. The registered `artifact_location` then names the remote
//! store — `r2://<bucket>/<job_id>/<artifact_manifest_hash>`.
//!
//! Configuration is the four `ATO_ARTIFACT_S3_*` env vars, ALL-OR-NOTHING: a
//! partial set is an operator error that stops the daemon at startup (never a
//! per-job surprise), and a fully absent set keeps the daemon byte-identical to
//! v1 (same-host `cas://` location, no packing, no upload). The local on-disk
//! job layout stays `{manifest.json, snapshot-manifest-v1.json?,
//! artifact-envelope-v1.json?, cas/}` either way — the tar.gz is a
//! transport artifact, removed after the upload attempt succeeds or fails.
//!
//! Shell-outs go through the snapshot crate's [`ImportCommandRunner`] seam
//! (`snapshot::docker_import::build`) — the same fake-able command seam the
//! Dockerfile import build uses, reused here rather than re-invented — so the
//! tar and curl command shapes and the retry loop are unit-testable.

use std::path::{Path, PathBuf};
use std::time::Duration;

use snapshot::{ARTIFACT_ENVELOPE_V1_FILENAME, SNAPSHOT_MANIFEST_V1_FILENAME};

pub use snapshot::docker_import::build::{ImportCommandRunner, SystemImportCommandRunner};

pub const ENV_ENDPOINT: &str = "ATO_ARTIFACT_S3_ENDPOINT";
pub const ENV_BUCKET: &str = "ATO_ARTIFACT_S3_BUCKET";
pub const ENV_ACCESS_KEY_ID: &str = "ATO_ARTIFACT_S3_ACCESS_KEY_ID";
pub const ENV_SECRET_ACCESS_KEY: &str = "ATO_ARTIFACT_S3_SECRET_ACCESS_KEY";

/// The one transport file per job (contract: key
/// `<job_id>/<artifact_manifest_hash>/artifact.tar.gz`).
pub const ARTIFACT_ARCHIVE_NAME: &str = "artifact.tar.gz";

/// Backoff BETWEEN upload attempts — attempts = `len + 1` = 3 total
/// (retries(3, backoff) per the ato#1002 contract).
const UPLOAD_BACKOFF: &[Duration] = &[Duration::from_secs(2), Duration::from_secs(5)];

/// The v1 same-host location, verbatim (`cas://<job_id>/<artifact_manifest_hash>`
/// names `<work>/<job_id>/{manifest.json, snapshot-manifest-v1.json?, cas/}` on the builder host itself).
/// Kept as a named function (not inline in `process_job`) so the absent-config
/// path is PINNED by a unit test — this string is registry data whose shape
/// must never drift.
pub fn cas_location(job_id: &str, artifact_manifest_hash: &str) -> String {
    format!("cas://{job_id}/{artifact_manifest_hash}")
}

/// The configured S3-compatible artifact store (endpoint + bucket + SigV4
/// credentials). Constructed all-or-nothing from the `ATO_ARTIFACT_S3_*` env.
pub struct ArtifactStore {
    /// Base endpoint URL, trailing `/` normalized away.
    endpoint: String,
    bucket: String,
    access_key_id: String,
    secret_access_key: String,
}

/// NEVER derive Debug here: the store carries live credentials, and this
/// daemon's own L4 no-secret gate exists precisely because builder credentials
/// leaking into logs/artifacts is the concrete threat. Only the secret is
/// redacted — endpoint/bucket/key-id are the useful non-secret ops facts.
impl std::fmt::Debug for ArtifactStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArtifactStore")
            .field("endpoint", &self.endpoint)
            .field("bucket", &self.bucket)
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"<redacted>")
            .finish()
    }
}

impl ArtifactStore {
    /// Read the four env vars. `Ok(None)` = fully absent (v1 behavior),
    /// `Ok(Some)` = fully present, `Err` = PARTIAL — a hard startup error.
    /// A set-but-blank var counts as absent (it cannot possibly work).
    pub fn from_env() -> Result<Option<ArtifactStore>, String> {
        let get = |key: &str| {
            std::env::var(key)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        ArtifactStore::from_parts(
            get(ENV_ENDPOINT),
            get(ENV_BUCKET),
            get(ENV_ACCESS_KEY_ID),
            get(ENV_SECRET_ACCESS_KEY),
        )
    }

    /// The pure all-or-nothing gate behind [`ArtifactStore::from_env`]
    /// (separated so the matrix is testable without touching process env).
    /// The error names the MISSING vars only — never a provided value.
    pub fn from_parts(
        endpoint: Option<String>,
        bucket: Option<String>,
        access_key_id: Option<String>,
        secret_access_key: Option<String>,
    ) -> Result<Option<ArtifactStore>, String> {
        let missing: Vec<&str> = [
            (ENV_ENDPOINT, endpoint.is_none()),
            (ENV_BUCKET, bucket.is_none()),
            (ENV_ACCESS_KEY_ID, access_key_id.is_none()),
            (ENV_SECRET_ACCESS_KEY, secret_access_key.is_none()),
        ]
        .iter()
        .filter(|(_, absent)| *absent)
        .map(|(key, _)| *key)
        .collect();
        match missing.len() {
            4 => Ok(None),
            0 => Ok(Some(ArtifactStore {
                endpoint: endpoint.unwrap().trim_end_matches('/').to_string(),
                bucket: bucket.unwrap(),
                access_key_id: access_key_id.unwrap(),
                secret_access_key: secret_access_key.unwrap(),
            })),
            _ => Err(format!(
                "artifact store is partially configured — set all four ATO_ARTIFACT_S3_* env vars to enable uploads, \
                 or none of them to keep same-host cas:// artifacts (missing: {})",
                missing.join(", ")
            )),
        }
    }

    /// The remote registry location: `r2://<bucket>/<job_id>/<artifact_manifest_hash>`.
    pub fn location(&self, job_id: &str, artifact_manifest_hash: &str) -> String {
        format!("r2://{}/{job_id}/{artifact_manifest_hash}", self.bucket)
    }

    /// The S3-compatible object URL the archive is PUT to:
    /// `<endpoint>/<bucket>/<job_id>/<artifact_manifest_hash>/artifact.tar.gz`
    /// — the same `<job_id>/<artifact_manifest_hash>` identity the registry's
    /// `r2://` location names, plus the fixed archive filename.
    fn object_url(&self, job_id: &str, artifact_manifest_hash: &str) -> String {
        format!(
            "{}/{}/{job_id}/{artifact_manifest_hash}/{ARTIFACT_ARCHIVE_NAME}",
            self.endpoint, self.bucket
        )
    }

    /// Package + upload one sealed job artifact, returning the REMOTE
    /// `artifact_location`. Runs BEFORE the sealed ack; any `Err` here must
    /// become a failed ack with stage `artifact_upload` (never
    /// sealed-without-bytes).
    pub fn pack_and_upload(
        &self,
        runner: &dyn ImportCommandRunner,
        jobdir: &Path,
        job_id: &str,
        artifact_manifest_hash: &str,
    ) -> Result<String, String> {
        self.pack_and_upload_with_backoff(
            runner,
            jobdir,
            job_id,
            artifact_manifest_hash,
            UPLOAD_BACKOFF,
        )
    }

    fn pack_and_upload_with_backoff(
        &self,
        runner: &dyn ImportCommandRunner,
        jobdir: &Path,
        job_id: &str,
        artifact_manifest_hash: &str,
        backoff: &[Duration],
    ) -> Result<String, String> {
        let archive = pack_artifact(runner, jobdir)?;
        let uploaded =
            self.upload_with_backoff(runner, &archive, job_id, artifact_manifest_hash, backoff);
        // The tar.gz is transport-only: remove it on success AND failure so the
        // local job layout stays {manifest.json, snapshot-manifest-v1.json?,
        // artifact-envelope-v1.json?, cas/} and a failed job never
        // leaves a stale archive to be confused for uploaded bytes.
        let _ = std::fs::remove_file(&archive);
        uploaded?;
        Ok(self.location(job_id, artifact_manifest_hash))
    }

    /// `curl --fail --aws-sigv4 "aws:amz:auto:s3" --user KEY:SECRET -T <file> <url>`,
    /// retried with backoff. argv-array exec — no shell, no quoting pitfalls.
    /// Credentials ride argv on this builder-local shell-out exactly per the
    /// ato#1002 contract; they must never ride an error message — failures
    /// report exit status + stderr tail + URL only.
    fn upload_with_backoff(
        &self,
        runner: &dyn ImportCommandRunner,
        archive: &Path,
        job_id: &str,
        artifact_manifest_hash: &str,
        backoff: &[Duration],
    ) -> Result<(), String> {
        let url = self.object_url(job_id, artifact_manifest_hash);
        let user = format!("{}:{}", self.access_key_id, self.secret_access_key);
        let archive_arg = archive.to_string_lossy();
        let attempts = backoff.len() + 1;
        let mut last = String::new();
        for attempt in 1..=attempts {
            if attempt > 1 {
                std::thread::sleep(backoff[attempt - 2]);
            }
            match runner.run(
                "curl",
                &[
                    "--fail",
                    "--aws-sigv4",
                    "aws:amz:auto:s3",
                    "--user",
                    &user,
                    "-T",
                    archive_arg.as_ref(),
                    &url,
                ],
            ) {
                Ok(out) if out.status == 0 => return Ok(()),
                Ok(out) => {
                    last = format!("curl exited {}: {}", out.status, stderr_tail(&out.stderr))
                }
                Err(e) => last = format!("spawn curl: {e}"),
            }
            eprintln!("[builder] artifact upload attempt {attempt}/{attempts} failed: {last}");
        }
        Err(format!(
            "artifact upload to {url} failed after {attempts} attempt(s): {last}"
        ))
    }
}

/// Package one job's sealed artifact as `<jobdir>/artifact.tar.gz` containing
/// exactly `manifest.json`, an optional v1 sidecar, and `cas/` at the archive root. The member list is
/// FIXED — never a glob, never the whole jobdir (which would swallow build
/// scratch like `rootfs.ext4` / verify overlays into the transport artifact).
/// argv-array exec (no shell), fail-closed on any nonzero tar exit.
pub fn pack_artifact(runner: &dyn ImportCommandRunner, jobdir: &Path) -> Result<PathBuf, String> {
    let archive = jobdir.join(ARTIFACT_ARCHIVE_NAME);
    let jobdir_arg = jobdir.to_string_lossy();
    let archive_arg = archive.to_string_lossy();
    let mut args = vec![
        "-C",
        jobdir_arg.as_ref(),
        "-czf",
        archive_arg.as_ref(),
        "manifest.json",
    ];
    if jobdir.join(SNAPSHOT_MANIFEST_V1_FILENAME).is_file() {
        args.push(SNAPSHOT_MANIFEST_V1_FILENAME);
    }
    if jobdir.join(ARTIFACT_ENVELOPE_V1_FILENAME).is_file() {
        args.push(ARTIFACT_ENVELOPE_V1_FILENAME);
    }
    args.push("cas");
    let out = runner
        .run("tar", &args)
        .map_err(|e| format!("spawn tar: {e}"))?;
    if out.status != 0 {
        return Err(format!(
            "pack {ARTIFACT_ARCHIVE_NAME}: tar exited {}: {}",
            out.status,
            stderr_tail(&out.stderr)
        ));
    }
    Ok(archive)
}

/// Last lines of a captured stderr for a fail-closed error message (mirrors the
/// import build's 12-line tail — enough to diagnose, small enough for a
/// failed-ack reason).
fn stderr_tail(stderr: &str) -> String {
    let lines: Vec<&str> = stderr.lines().rev().take(12).collect();
    lines.into_iter().rev().collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use snapshot::docker_import::build::ImportCommandOutput;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// Sequential scripted fake: pops one response per invocation (the upload
    /// flow is strictly ordered: one tar, then N curls) and records every full
    /// command line for shape assertions.
    struct FakeRunner {
        script: Mutex<VecDeque<Result<ImportCommandOutput, std::io::ErrorKind>>>,
        calls: Mutex<Vec<String>>,
    }

    impl FakeRunner {
        fn new(script: Vec<Result<ImportCommandOutput, std::io::ErrorKind>>) -> Self {
            FakeRunner {
                script: Mutex::new(script.into()),
                calls: Mutex::new(Vec::new()),
            }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl ImportCommandRunner for FakeRunner {
        fn run(&self, program: &str, args: &[&str]) -> std::io::Result<ImportCommandOutput> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("{program} {}", args.join(" ")));
            match self.script.lock().unwrap().pop_front() {
                Some(Ok(out)) => Ok(out),
                Some(Err(kind)) => Err(kind.into()),
                None => panic!("unscripted command: {program}"),
            }
        }
    }

    fn ok() -> Result<ImportCommandOutput, std::io::ErrorKind> {
        Ok(ImportCommandOutput {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    }

    fn exit(status: i32, stderr: &str) -> Result<ImportCommandOutput, std::io::ErrorKind> {
        Ok(ImportCommandOutput {
            status,
            stdout: String::new(),
            stderr: stderr.to_string(),
        })
    }

    fn store() -> ArtifactStore {
        ArtifactStore::from_parts(
            Some("https://acct.r2.example.com".into()),
            Some("ato-artifacts".into()),
            Some("AKIDEXAMPLE".into()),
            Some("secret-value-7f3a9c".into()),
        )
        .unwrap()
        .unwrap()
    }

    const ZERO_BACKOFF: &[Duration] = &[Duration::ZERO, Duration::ZERO];

    // ── config all-or-nothing matrix ─────────────────────────────────────────

    #[test]
    fn absent_config_is_none_and_the_v1_cas_location_is_unchanged() {
        // All four vars absent ⇒ no store ⇒ process_job keeps the v1 same-host
        // location. Both halves of that behavior are pinned here.
        assert!(
            ArtifactStore::from_parts(None, None, None, None)
                .unwrap()
                .is_none()
        );
        // The EXACT pre-ato#1002 string (was inline `format!("cas://{}/{}", …)`
        // in process_job) — registry data, must never drift.
        assert_eq!(
            cas_location("job_1", "blake3:abc"),
            "cas://job_1/blake3:abc"
        );
    }

    #[test]
    fn full_config_parses_and_normalizes_the_endpoint() {
        let s = ArtifactStore::from_parts(
            Some("https://acct.r2.example.com/".into()), // trailing slash normalized
            Some("ato-artifacts".into()),
            Some("AKIDEXAMPLE".into()),
            Some("secret-value-7f3a9c".into()),
        )
        .unwrap()
        .expect("fully configured store");
        // The object key carries the immutable identity — <job_id>/<hash>/artifact.tar.gz.
        assert_eq!(
            s.object_url("job_9", "blake3:abc"),
            "https://acct.r2.example.com/ato-artifacts/job_9/blake3:abc/artifact.tar.gz"
        );
    }

    #[test]
    fn partial_config_is_a_hard_error_naming_only_the_missing_vars() {
        // Every combination except all-absent / all-present must ERROR (the
        // daemon refuses to start half-configured rather than silently falling
        // back to cas:// artifacts that a remote runner cannot fetch).
        for mask in 1u8..15 {
            let part = |bit: u8, v: &str| (mask & bit != 0).then(|| v.to_string());
            let err = ArtifactStore::from_parts(
                part(1, "https://e"),
                part(2, "bucket"),
                part(4, "AKIDEXAMPLE"),
                part(8, "secret-value-7f3a9c"),
            )
            .unwrap_err();
            assert!(err.contains("partially configured"), "{err}");
            // Names each missing var…
            if mask & 2 == 0 {
                assert!(err.contains(ENV_BUCKET), "{err}");
            }
            if mask & 8 == 0 {
                assert!(err.contains(ENV_SECRET_ACCESS_KEY), "{err}");
            }
            // …and never echoes a provided value (the secret above all).
            assert!(!err.contains("secret-value-7f3a9c"), "{err}");
        }
    }

    #[test]
    fn debug_never_prints_the_secret() {
        let dbg = format!("{:?}", store());
        assert!(dbg.contains("ato-artifacts"), "{dbg}");
        assert!(dbg.contains("<redacted>"), "{dbg}");
        assert!(!dbg.contains("secret-value-7f3a9c"), "{dbg}");
    }

    // ── pack command shape ───────────────────────────────────────────────────

    #[test]
    // snapshot-builder only ever runs on Linux in production (it shells out to
    // `tar`/`curl` and needs the Firecracker/KVM toolchain to be meaningful at
    // all); this asserts the EXACT argv strings including path separators,
    // which legitimately follow the host platform via `PathBuf::join` —
    // Windows would (correctly, for a Windows host) produce a `\`-separated
    // argv here, which is not what this test is proving.
    #[cfg(unix)]
    fn pack_runs_the_exact_tar_command() {
        let r = FakeRunner::new(vec![ok()]);
        let archive = pack_artifact(&r, Path::new("/work/job_9")).unwrap();
        assert_eq!(archive, PathBuf::from("/work/job_9/artifact.tar.gz"));
        // Exactly manifest.json + cas at the archive root — fixed member list.
        assert_eq!(
            r.calls(),
            vec!["tar -C /work/job_9 -czf /work/job_9/artifact.tar.gz manifest.json cas"]
        );
    }

    #[test]
    #[cfg(unix)]
    fn pack_includes_snapshot_v1_manifest_when_present() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(SNAPSHOT_MANIFEST_V1_FILENAME), b"{}").unwrap();
        std::fs::write(dir.path().join(ARTIFACT_ENVELOPE_V1_FILENAME), b"{}").unwrap();
        let runner = FakeRunner::new(vec![ok()]);

        pack_artifact(&runner, dir.path()).unwrap();

        let command = runner.calls().into_iter().next().unwrap();
        assert!(
            command
                .ends_with("manifest.json snapshot-manifest-v1.json artifact-envelope-v1.json cas"),
            "{command}"
        );
    }

    #[test]
    fn pack_fails_closed_on_nonzero_tar_exit() {
        let r = FakeRunner::new(vec![exit(
            2,
            "tar: manifest.json: Cannot stat: No such file or directory",
        )]);
        let err = pack_artifact(&r, Path::new("/work/job_9")).unwrap_err();
        assert!(err.contains("tar exited 2"), "{err}");
        assert!(err.contains("Cannot stat"), "{err}");

        let r = FakeRunner::new(vec![Err(std::io::ErrorKind::NotFound)]);
        let err = pack_artifact(&r, Path::new("/work/job_9")).unwrap_err();
        assert!(err.contains("spawn tar"), "{err}");
    }

    // ── upload retry/backoff ─────────────────────────────────────────────────

    #[test]
    #[cfg(unix)] // same host-path-separator reasoning as pack_runs_the_exact_tar_command
    fn upload_shape_is_the_contract_curl_invocation() {
        let s = store();
        let r = FakeRunner::new(vec![ok(), ok()]); // tar, then curl first-try success
        let loc = s
            .pack_and_upload_with_backoff(
                &r,
                Path::new("/work/job_9"),
                "job_9",
                "blake3:abc",
                ZERO_BACKOFF,
            )
            .unwrap();
        assert_eq!(loc, "r2://ato-artifacts/job_9/blake3:abc");
        assert_eq!(
            r.calls()[1],
            "curl --fail --aws-sigv4 aws:amz:auto:s3 --user AKIDEXAMPLE:secret-value-7f3a9c \
             -T /work/job_9/artifact.tar.gz https://acct.r2.example.com/ato-artifacts/job_9/blake3:abc/artifact.tar.gz"
        );
    }

    #[test]
    fn upload_retries_twice_then_succeeds() {
        let s = store();
        // tar ok; curl: 2 failures (HTTP error, spawn error) then success ⇒ the
        // job still SEALS with the remote location.
        let r = FakeRunner::new(vec![
            ok(),
            exit(22, "The requested URL returned error: 500"),
            Err(std::io::ErrorKind::NotFound),
            ok(),
        ]);
        let loc = s
            .pack_and_upload_with_backoff(
                &r,
                Path::new("/work/job_9"),
                "job_9",
                "blake3:abc",
                ZERO_BACKOFF,
            )
            .unwrap();
        assert_eq!(loc, "r2://ato-artifacts/job_9/blake3:abc");
        let calls = r.calls();
        assert_eq!(calls.len(), 4, "one tar + three curl attempts: {calls:?}");
        assert!(calls[0].starts_with("tar "), "{calls:?}");
        assert!(
            calls[1..].iter().all(|c| c.starts_with("curl ")),
            "{calls:?}"
        );
    }

    #[test]
    fn upload_exhausting_all_attempts_is_an_error_never_a_location() {
        let s = store();
        let r = FakeRunner::new(vec![
            ok(), // tar
            exit(22, "The requested URL returned error: 503"),
            exit(22, "The requested URL returned error: 503"),
            exit(7, "Failed to connect"),
        ]);
        let err = s
            .pack_and_upload_with_backoff(
                &r,
                Path::new("/work/job_9"),
                "job_9",
                "blake3:abc",
                ZERO_BACKOFF,
            )
            .unwrap_err();
        // process_job maps this Err to failure_stage="artifact_upload" — a
        // failed ack, so no sealed ack (and no r2:// location) can exist.
        assert!(err.contains("failed after 3 attempt(s)"), "{err}");
        assert!(err.contains("curl exited 7"), "{err}");
        // The reason reaches the failed ack: URL yes, credentials never.
        assert!(
            err.contains(
                "https://acct.r2.example.com/ato-artifacts/job_9/blake3:abc/artifact.tar.gz"
            ),
            "{err}"
        );
        assert!(!err.contains("secret-value-7f3a9c"), "{err}");
        assert_eq!(r.calls().len(), 4, "exactly one tar + three curl attempts");
    }

    #[test]
    fn default_backoff_gives_exactly_three_attempts() {
        // retries(3, backoff) per the ato#1002 contract: two sleeps ⇒ three tries.
        assert_eq!(UPLOAD_BACKOFF.len() + 1, 3);
    }

    #[test]
    fn pack_failure_never_reaches_curl() {
        let s = store();
        let r = FakeRunner::new(vec![exit(
            1,
            "tar: cas: Cannot stat: No such file or directory",
        )]);
        let err = s
            .pack_and_upload_with_backoff(
                &r,
                Path::new("/work/job_9"),
                "job_9",
                "blake3:abc",
                ZERO_BACKOFF,
            )
            .unwrap_err();
        assert!(err.contains("tar exited 1"), "{err}");
        assert_eq!(
            r.calls().len(),
            1,
            "no upload may be attempted for an unpacked artifact"
        );
    }
}
