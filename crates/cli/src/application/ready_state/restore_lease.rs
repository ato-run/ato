//! Track E (#912): runner-side `restore_snapshot` lease — the fetch/verify half.
//!
//! A Connected/Managed runner receives a `restore_snapshot` lease from ato-api
//! (Track D, ato-api#159). The lease is **reference-only**: `artifact_location` is a
//! HINT, and the identity fields exist so the runner can **verify the artifact it
//! fetched is exactly the one the registry sealed** before restoring. This module is the
//! pure, host-independent core (parse + locate + verify); the restore/expose/report/
//! teardown orchestration lives in `runner_agent`.
//!
//! The critical gate `backend.restore` does NOT provide: recomputing the sealed
//! manifest's blake3 id and requiring it to equal the lease's `artifact_manifest_hash`.
//! Without it, a runner would trust whatever manifest happened to be on disk at the
//! CAS path — this module closes that hole (fail-closed).
//!
//! ato#1002 (Snapshot Serving v1): `artifact_location` may now also be
//! `r2://<bucket>/<job_id>/<hash>` — a remote object store. The bytes are fetched via
//! the lease's short-lived presigned `artifact_fetch_url` into the SAME
//! `<artifact_root>/<job_id>/{manifest.json, snapshot-manifest-v1.json?,
//! artifact-envelope-v1.json?, cas/}` layout ([`ensure_artifact_local`]),
//! then verified by the legacy manifest gate and, for an explicit Capsule v1
//! restore lease, [`load_verified_v1_artifact`]. The fetch only lands bytes for
//! those gates — it adds no parallel trust path.

use std::path::{Path, PathBuf};

use capsule::execution_contract::{EXECUTION_CONTRACT_V1_SCHEMA, ExecutionId};
use capsule::snapshot_manifest::{SNAPSHOT_MANIFEST_V1_SCHEMA, SnapshotManifestV1};
use protocol::session_surface::{
    AcceptedSessionSurface, ClientSessionSurfaceCapabilities, PIXEL_STREAM_PROFILE,
    RunnerSessionSurfaceCapabilities, SESSION_SURFACE_CONTRACT_VERSION, SessionSurfaceDescriptor,
    SessionSurfaceKind, SessionSurfaceRequirement, SessionSurfaceTransport,
    SupportedSessionSurface, WEB_SURFACE_PROFILE, negotiate_session_surface,
};
use snapshot::{
    ARTIFACT_ENVELOPE_V1_FILENAME, ARTIFACT_ENVELOPE_V1_SCHEMA, ArtifactEnvelopeV1,
    ReadyStateManifest, SNAPSHOT_MANIFEST_V1_FILENAME,
};

/// Lease kind for restoring a sealed Ready-State snapshot (matches ato-api's
/// `RESTORE_SNAPSHOT_LEASE_KIND`).
pub(crate) const RESTORE_SNAPSHOT_LEASE_KIND: &str = "restore_snapshot";

/// v1.2 PR 3e: lease kind for restoring a SUPERVISOR (binding-required) snapshot.
/// A separate kind — not an additive field — so the control plane capability-gates
/// dispatch on `supported_lease_kinds` and an older runner is NEVER handed a
/// binding artifact it cannot serve. Payload shape is identical to
/// `restore_snapshot`; the binding names come from the sealed manifest
/// (`supervisor_build.binding_names`), the single source of truth — a lease field
/// would not be trusted.
pub(crate) const RESTORE_SNAPSHOT_WITH_BINDINGS_LEASE_KIND: &str = "restore_snapshot_with_bindings";

/// Public Preview Runner v0 (ato#1006, UNIT C): lease kind for a login-free,
/// time-limited preview restore. Wire-identical to `restore_snapshot` (a NO-BINDING
/// artifact — see [`classify_restore_artifact`]) PLUS a required hard TTL
/// (`max_duration_secs`) and optional input-idle timeout (`idle_timeout_secs`). A
/// separate kind — not an additive field — so the control plane capability-gates
/// dispatch on `supported_lease_kinds`: only a runner that opted in
/// (`ATO_RUNNER_PREVIEW=1`) ever advertises it, and only a snapshot the server
/// re-checked as free-preview eligible is ever minted one.
pub(crate) const RESTORE_SNAPSHOT_PREVIEW_LEASE_KIND: &str = "restore_snapshot_preview";

/// The reference-only identity a `restore_snapshot` lease carries. No secrets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RestoreSnapshotCommand {
    pub snapshot_id: String,
    pub capsule_id: String,
    pub target_label: String,
    pub profile: String,
    pub artifact_location: String,
    /// ato#1002: short-lived presigned GET for an `r2://` artifact — set by ato-api
    /// only when the artifact lives in the remote object store. A HINT like
    /// `artifact_location`: the fetched bytes still pass the full
    /// [`load_and_verify_manifest`] gate before anything is restored.
    pub artifact_fetch_url: Option<String>,
    pub artifact_manifest_hash: String,
    pub capsule_manifest_hash: String,
    pub execution_id: String,
    /// Explicit Capsule v1 execution identity schema, when the lease references
    /// a v1 Snapshot. `None` ⇒ legacy (a bare `execution_id` never implies v1).
    pub execution_identity_schema: Option<String>,
    /// Required alongside `execution_identity_schema` — see
    /// [`parse_restore_snapshot_command`]'s v1 completeness check.
    pub snapshot_manifest_schema: Option<String>,
    pub snapshot_manifest_id: Option<String>,
    pub artifact_envelope_schema: Option<String>,
    pub artifact_envelope_id: Option<String>,
    pub runner_class_id: String,
    pub snapshot_backend: String,
    pub healthcheck_url_path: Option<String>,
    /// Immutable descriptor selected by capsule × client × runner negotiation.
    /// Access URLs and grants are intentionally absent from the lease contract.
    /// Legacy Web leases omit this field and `surface_contract_version` together.
    pub session_surface: Option<SessionSurfaceDescriptor>,
    pub surface_contract_version: Option<String>,
    /// Control-plane session binding for the surface gateway assertion scope.
    /// Required only for an explicit canonical surface lease.
    pub session_id: Option<String>,
    /// Launch-client capability set used in the API-side selection. Explicit
    /// surface leases must carry it so the runner can repeat the intersection.
    pub accepted_session_surfaces: Option<Vec<AcceptedSessionSurface>>,
    /// v1.2 PR 3e: true iff the lease kind is `restore_snapshot_with_bindings`.
    /// The kind PROMISES a supervisor artifact; `classify_restore_artifact`
    /// fail-closes any kind↔artifact mismatch in either direction.
    pub with_bindings: bool,
    /// ato#1006 (UNIT C): true iff the lease kind is `restore_snapshot_preview`.
    /// A preview lease restores exactly a no-binding artifact (`with_bindings` is
    /// always `false` for it) but arms a hard max-duration TTL in the hold loop.
    pub is_preview: bool,
    /// ato#1006 (UNIT C): the preview lane's HARD wall-clock cap, in seconds.
    /// REQUIRED (non-empty) for a `restore_snapshot_preview` lease and `None` for
    /// every other kind — an additive/optional wire field (`#[serde(default)]`
    /// semantics), byte-compatible for non-preview leases which never carry it.
    pub max_duration_secs: Option<u64>,
    /// ato#1006 (UNIT C): the preview lane's idle timeout, in seconds. For Pixel
    /// surfaces, parsed RFB keyboard/pointer input resets the timer; framebuffer
    /// requests, keepalives, and outbound frame activity do not. Optional on the
    /// wire; `None` for non-preview leases.
    pub idle_timeout_secs: Option<u64>,
    /// P3b AI-keyless: the run this lease dispatches. ato-api has always sent
    /// `run_id` on the restore command; parsing it (additive, optional — absent
    /// on a hand-rolled lease → None) lets the runner ask ato-api for the run's
    /// AI grant at claim. Never an identity/verification input.
    pub run_id: Option<String>,
}

/// Parse + validate a `restore_snapshot` / `restore_snapshot_with_bindings` /
/// `restore_snapshot_preview` lease command. Every identity field is required and
/// non-empty (a lease missing one is refused, never restored blind).
///
/// ato#1006 (UNIT C): a `restore_snapshot_preview` lease additionally carries
/// `max_duration_secs` (REQUIRED — fail closed if absent, a preview must never run
/// unbounded) and `idle_timeout_secs` (optional). Both are ignored for the other
/// kinds, which never carry them (byte-compatible).
pub(crate) fn parse_restore_snapshot_command(
    command: &serde_json::Value,
) -> std::result::Result<RestoreSnapshotCommand, (String, String)> {
    let err = |m: &str| ("invalid_restore_lease".to_string(), m.to_string());
    let kind = command.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    // A preview lease restores exactly a no-binding artifact — `with_bindings` is
    // false for it — but is flagged `is_preview` so the hold loop arms its TTL.
    let (with_bindings, is_preview) = match kind {
        RESTORE_SNAPSHOT_LEASE_KIND => (false, false),
        RESTORE_SNAPSHOT_WITH_BINDINGS_LEASE_KIND => (true, false),
        RESTORE_SNAPSHOT_PREVIEW_LEASE_KIND => (false, true),
        _ => {
            return Err(err(&format!(
                "not a restore_snapshot lease (kind {kind:?})"
            )));
        }
    };
    let req = |k: &str| -> std::result::Result<String, (String, String)> {
        command
            .get(k)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                err(&format!(
                    "restore_snapshot lease is missing required field {k:?}"
                ))
            })
    };
    // Additive, optional duration fields. `#[serde(default)]`-style: absent → None.
    let max_duration_secs = command
        .get("max_duration_secs")
        .and_then(serde_json::Value::as_u64);
    let idle_timeout_secs = command
        .get("idle_timeout_secs")
        .and_then(serde_json::Value::as_u64);
    // Fail closed: a preview lease MUST carry a hard max-duration cap. Without it the
    // preview would serve unbounded — refuse rather than run without the TTL.
    if is_preview && max_duration_secs.is_none() {
        return Err(err(
            "restore_snapshot_preview lease is missing required field \"max_duration_secs\"",
        ));
    }
    // The additive migration permits one legacy shape only: both fields absent,
    // which means an existing Web restore. Once either canonical field is present,
    // both are authoritative and malformed/unknown values fail closed. In
    // particular, an explicit pixel_stream descriptor can never fall back to Web.
    let fields = command.as_object();
    let surface_present = fields.is_some_and(|fields| fields.contains_key("session_surface"));
    let version_present =
        fields.is_some_and(|fields| fields.contains_key("surface_contract_version"));
    let (surface_contract_version, session_surface) = match (version_present, surface_present) {
        (false, false) => (None, None),
        (false, true) => {
            return Err(err(
                "restore_snapshot lease has session_surface but is missing surface_contract_version",
            ));
        }
        (true, false) => {
            return Err(err(
                "restore_snapshot lease has surface_contract_version but is missing session_surface",
            ));
        }
        (true, true) => {
            let version = command
                .get("surface_contract_version")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| err("surface_contract_version must be a non-empty string"))?;
            if version != SESSION_SURFACE_CONTRACT_VERSION {
                return Err(err(&format!(
                    "unsupported surface_contract_version {version:?}"
                )));
            }
            let descriptor: SessionSurfaceDescriptor = serde_json::from_value(
                command
                    .get("session_surface")
                    .cloned()
                    .expect("field presence was checked"),
            )
            .map_err(|error| err(&format!("malformed session_surface: {error}")))?;
            descriptor
                .validate()
                .map_err(|error| err(&format!("invalid session_surface: {error}")))?;
            (Some(version.to_string()), Some(descriptor))
        }
    };
    let accepted_present =
        fields.is_some_and(|fields| fields.contains_key("accepted_session_surfaces"));
    let accepted_session_surfaces = if accepted_present {
        let accepted: Vec<AcceptedSessionSurface> = serde_json::from_value(
            command
                .get("accepted_session_surfaces")
                .cloned()
                .expect("field presence was checked"),
        )
        .map_err(|error| err(&format!("malformed accepted_session_surfaces: {error}")))?;
        ClientSessionSurfaceCapabilities {
            accepted_session_surfaces: Some(accepted.clone()),
        }
        .validate()
        .map_err(|error| err(&format!("invalid accepted_session_surfaces: {error}")))?;
        Some(accepted)
    } else {
        None
    };
    if session_surface.is_some() && accepted_session_surfaces.is_none() {
        return Err(err(
            "explicit session_surface lease is missing accepted_session_surfaces",
        ));
    }
    let session_id = if fields.is_some_and(|fields| fields.contains_key("session_id")) {
        Some(
            command
                .get("session_id")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .ok_or_else(|| err("session_id must be a non-empty string"))?,
        )
    } else {
        None
    };
    if session_surface.is_some() && session_id.is_none() {
        return Err(err("explicit session_surface lease is missing session_id"));
    }
    let optional_string = |field: &str| {
        command
            .get(field)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let execution_identity_schema = optional_string("execution_identity_schema");
    let snapshot_manifest_schema = optional_string("snapshot_manifest_schema");
    let snapshot_manifest_id = optional_string("snapshot_manifest_id");
    let artifact_envelope_schema = optional_string("artifact_envelope_schema");
    let artifact_envelope_id = optional_string("artifact_envelope_id");
    match execution_identity_schema.as_deref() {
        None => {
            if snapshot_manifest_schema.is_some()
                || snapshot_manifest_id.is_some()
                || artifact_envelope_schema.is_some()
                || artifact_envelope_id.is_some()
            {
                return Err(err(
                    "legacy restore lease must not carry partial Capsule v1 artifact metadata",
                ));
            }
        }
        Some(EXECUTION_CONTRACT_V1_SCHEMA) => {
            if snapshot_manifest_schema.as_deref() != Some(SNAPSHOT_MANIFEST_V1_SCHEMA)
                || artifact_envelope_schema.as_deref() != Some(ARTIFACT_ENVELOPE_V1_SCHEMA)
                || snapshot_manifest_id.is_none()
                || artifact_envelope_id.is_none()
            {
                return Err(err(
                    "Capsule v1 restore lease requires snapshot manifest/envelope schemas and ids",
                ));
            }
        }
        Some(other) => {
            return Err(err(&format!(
                "unsupported execution_identity_schema {other:?}"
            )));
        }
    }
    Ok(RestoreSnapshotCommand {
        snapshot_id: req("snapshot_id")?,
        capsule_id: req("capsule_id")?,
        target_label: req("target_label")?,
        profile: req("profile")?,
        artifact_location: req("artifact_location")?,
        artifact_fetch_url: command
            .get("artifact_fetch_url")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty()),
        artifact_manifest_hash: req("artifact_manifest_hash")?,
        capsule_manifest_hash: req("capsule_manifest_hash")?,
        execution_id: req("execution_id")?,
        execution_identity_schema,
        snapshot_manifest_schema,
        snapshot_manifest_id,
        artifact_envelope_schema,
        artifact_envelope_id,
        runner_class_id: req("runner_class_id")?,
        snapshot_backend: req("snapshot_backend")?,
        healthcheck_url_path: command
            .get("healthcheck_url_path")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty()),
        session_surface,
        surface_contract_version,
        session_id,
        accepted_session_surfaces,
        with_bindings,
        is_preview,
        max_duration_secs,
        idle_timeout_secs,
        run_id: command
            .get("run_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    })
}

fn local_runner_surface_capabilities(
    pixel_surface_enabled: bool,
) -> RunnerSessionSurfaceCapabilities {
    let mut supported = vec![SupportedSessionSurface {
        kind: SessionSurfaceKind::Web,
        profiles: Some(vec![WEB_SURFACE_PROFILE.to_string()]),
        transports: Some(vec![SessionSurfaceTransport::Https]),
    }];
    if pixel_surface_enabled {
        supported.push(SupportedSessionSurface {
            kind: SessionSurfaceKind::PixelStream,
            profiles: Some(vec![PIXEL_STREAM_PROFILE.to_string()]),
            transports: Some(vec![SessionSurfaceTransport::RfbWebsocket]),
        });
    }
    RunnerSessionSurfaceCapabilities {
        supported_session_surfaces: Some(supported),
    }
}

fn verify_surface_negotiation(
    manifest: &ReadyStateManifest,
    cmd: &RestoreSnapshotCommand,
    pixel_surface_enabled: bool,
) -> std::result::Result<(), String> {
    if let Some(requirement) = &manifest.surface_requirement {
        requirement
            .validate()
            .map_err(|error| format!("artifact surface_requirement is invalid: {error}"))?;
    }

    let Some(descriptor) = &cmd.session_surface else {
        return match &manifest.surface_requirement {
            // The migration explicitly permits a Web artifact/lease pair to omit
            // canonical fields. Non-Web artifacts must never inherit that fallback.
            None => Ok(()),
            Some(requirement) if requirement.kind == SessionSurfaceKind::Web => Ok(()),
            Some(requirement) => Err(format!(
                "artifact requires {:?} but lease omitted session_surface",
                requirement.kind
            )),
        };
    };

    if cmd.surface_contract_version.as_deref() != Some(SESSION_SURFACE_CONTRACT_VERSION) {
        return Err("explicit session_surface has no supported surface_contract_version".into());
    }
    if cmd
        .session_id
        .as_deref()
        .is_none_or(|session_id| session_id.trim().is_empty())
    {
        return Err("explicit session_surface has no valid session_id binding".into());
    }
    let legacy_web_requirement;
    let requirement = match manifest.surface_requirement.as_ref() {
        Some(requirement) => requirement,
        None if descriptor.kind() == SessionSurfaceKind::Web => {
            legacy_web_requirement = SessionSurfaceRequirement {
                kind: SessionSurfaceKind::Web,
                profiles: Some(vec![WEB_SURFACE_PROFILE.to_string()]),
            };
            &legacy_web_requirement
        }
        None => {
            return Err(format!(
                "legacy artifact without surface_requirement accepts only an explicit Web surface, not {:?}",
                descriptor.kind()
            ));
        }
    };
    let accepted = cmd.accepted_session_surfaces.clone().ok_or_else(|| {
        "explicit session_surface lease omitted accepted_session_surfaces".to_string()
    })?;
    let selected = negotiate_session_surface(
        requirement,
        &ClientSessionSurfaceCapabilities {
            accepted_session_surfaces: Some(accepted),
        },
        &local_runner_surface_capabilities(pixel_surface_enabled),
    )
    .map_err(|error| format!("runner surface renegotiation failed: {error}"))?;
    let claimed = descriptor
        .as_selected_surface()
        .map_err(|error| format!("lease session_surface is invalid: {error}"))?;
    if claimed != selected {
        return Err(format!(
            "lease session_surface selection {claimed:?} does not match runner renegotiation {selected:?}"
        ));
    }
    Ok(())
}

/// The on-disk location of a fetched artifact: both manifest generations beside a `cas/` dir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArtifactPaths {
    pub manifest_json: PathBuf,
    pub snapshot_manifest_v1_json: PathBuf,
    pub artifact_envelope_v1_json: PathBuf,
    pub cas_dir: PathBuf,
}

/// Resolve an artifact location to on-disk paths under `artifact_root`. Every host
/// uses the SAME local layout
/// `<artifact_root>/<job_id>/{manifest.json, snapshot-manifest-v1.json?, cas/}`:
/// - `cas://<job_id>/<artifact_hash>` — v1 same-host: the builder already wrote it;
/// - `r2://<bucket>/<job_id>/<artifact_hash>` — ato#1002 remote object store: the
///   bytes may still need fetching ([`ensure_artifact_local`] owns that; this
///   function is pure path mapping and touches no filesystem).
///
/// Fail-closed: any other scheme (incl. bare `https://`) is rejected, the job
/// segment must be a single NORMAL path component (no `/`, `..`, `.`, absolute
/// escape, or Windows drive prefix), and the resolved dir must stay strictly
/// under `artifact_root`.
pub(crate) fn locate_artifact(
    artifact_location: &str,
    artifact_root: &Path,
) -> std::result::Result<ArtifactPaths, (String, String)> {
    let err = |m: String| ("artifact_unavailable".to_string(), m);
    let job = if let Some(rest) = artifact_location.strip_prefix("cas://") {
        rest.split('/').next().unwrap_or("")
    } else if let Some(rest) = artifact_location.strip_prefix("r2://") {
        // r2://<bucket>/<job_id>/<hash> — the bucket names the builder's upload
        // target; only <job_id> shapes the local path.
        let mut parts = rest.split('/');
        if parts.next().unwrap_or("").is_empty() {
            return Err(err(format!(
                "missing bucket segment in {artifact_location:?}"
            )));
        }
        parts.next().unwrap_or("")
    } else {
        return Err(err(format!(
            "unsupported artifact scheme in {artifact_location:?} (cas:// and r2:// only)"
        )));
    };
    // Exactly ONE Normal component: rejects empty, absolute paths, "." (CurDir —
    // which would resolve the job dir to artifact_root ITSELF, and the r2:// fetch
    // path clears a pre-existing job dir before publishing), and Windows drive
    // prefixes like "C:" (a Prefix component that `root.join` would escape with).
    if job.is_empty()
        || job.contains("..")
        || job.contains('\\')
        || !matches!(
            Path::new(job).components().collect::<Vec<_>>().as_slice(),
            [std::path::Component::Normal(_)]
        )
    {
        return Err(err(format!(
            "unsafe artifact job segment in {artifact_location:?}"
        )));
    }
    let dir = artifact_root.join(job);
    Ok(ArtifactPaths {
        manifest_json: dir.join("manifest.json"),
        snapshot_manifest_v1_json: dir.join(SNAPSHOT_MANIFEST_V1_FILENAME),
        artifact_envelope_v1_json: dir.join(ARTIFACT_ENVELOPE_V1_FILENAME),
        cas_dir: dir.join("cas"),
    })
}

/// ato#1002: make an artifact's bytes present at the [`locate_artifact`] paths,
/// fetching them for a remote `r2://` location when needed. `cas://` is untouched
/// (same-host: the builder already wrote the bytes — behavior identical to before).
///
/// For `r2://`:
/// - if `<artifact_root>/<job_id>/manifest.json` already exists, use it (idempotent —
///   a re-dispatched lease or a restart never re-downloads);
/// - else the lease MUST have carried `artifact_fetch_url` (a short-lived presigned
///   GET). The archive is downloaded to a temp file inside `artifact_root` (same
///   filesystem), safe-extracted ([`safe_extract_artifact_tar_gz`]) into a temp dir,
///   then atomically renamed into place — the job dir only ever appears complete.
///
/// Byte VERIFICATION stays in [`load_and_verify_manifest`] plus the explicit-v1
/// [`load_verified_v1_artifact`] envelope gate — this function adds no parallel
/// trust path.
/// Error messages never include the URL: a presigned GET carries its authorization
/// in the query string.
pub(crate) async fn ensure_artifact_local(
    client: &reqwest::Client,
    artifact_location: &str,
    artifact_root: &Path,
    artifact_fetch_url: Option<&str>,
    max_fetch_bytes: u64,
) -> std::result::Result<ArtifactPaths, (String, String)> {
    let err = |m: String| ("artifact_unavailable".to_string(), m);
    let paths = locate_artifact(artifact_location, artifact_root)?;
    if !artifact_location.starts_with("r2://") {
        return Ok(paths);
    }
    if paths.manifest_json.exists() {
        return Ok(paths);
    }
    let url = artifact_fetch_url.ok_or_else(|| {
        err(format!(
            "remote artifact {artifact_location:?} is not on this host and the lease carried \
             no artifact_fetch_url"
        ))
    })?;
    std::fs::create_dir_all(artifact_root).map_err(|e| {
        err(format!(
            "create artifact root {}: {e}",
            artifact_root.display()
        ))
    })?;
    // Temp file + staging dir live INSIDE artifact_root so the final rename is an
    // atomic same-filesystem move (never a cross-device copy).
    let archive = tempfile::Builder::new()
        .prefix(".artifact-fetch-")
        .suffix(".tar.gz")
        .tempfile_in(artifact_root)
        .map_err(|e| err(format!("create artifact download temp file: {e}")))?;
    download_artifact_archive(client, url, archive.path(), max_fetch_bytes)
        .await
        .map_err(err)?;
    let staging = tempfile::Builder::new()
        .prefix(".artifact-extract-")
        .tempdir_in(artifact_root)
        .map_err(|e| err(format!("create artifact staging dir: {e}")))?;
    {
        // Extraction can decompress GiBs — keep it off the async workers.
        let tar_gz = archive.path().to_path_buf();
        let dest = staging.path().to_path_buf();
        tokio::task::spawn_blocking(move || {
            safe_extract_artifact_tar_gz(&tar_gz, &dest, max_fetch_bytes)
        })
        .await
        .map_err(|e| err(format!("artifact extraction task failed: {e}")))?
        .map_err(err)?;
    }
    let job_dir = paths
        .manifest_json
        .parent()
        .expect("locate_artifact paths always have a job dir")
        .to_path_buf();
    // Atomic publish. A leftover job dir here has no manifest.json (checked above) —
    // a crashed partial extract from a PREVIOUS layout, safe to replace.
    if job_dir.exists() {
        std::fs::remove_dir_all(&job_dir).map_err(|e| {
            err(format!(
                "clear partial artifact dir {}: {e}",
                job_dir.display()
            ))
        })?;
    }
    let staging = staging.keep(); // rename takes ownership of the dir
    if let Err(e) = std::fs::rename(&staging, &job_dir) {
        let _ = std::fs::remove_dir_all(&staging);
        // A concurrent restore of the same job may have published first — then its
        // bytes are the same sealed artifact and the verify gate still runs.
        if !paths.manifest_json.exists() {
            return Err(err(format!(
                "publish artifact dir {}: {e}",
                job_dir.display()
            )));
        }
    }
    Ok(paths)
}

/// Stream a presigned GET to `dest`, capped at `max_bytes`. The URL is never echoed
/// into errors (reqwest errors are stripped via `without_url`).
async fn download_artifact_archive(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    max_bytes: u64,
) -> std::result::Result<(), String> {
    use futures::StreamExt;
    use std::io::Write;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("artifact fetch request failed: {}", e.without_url()))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("artifact fetch returned HTTP {status}"));
    }
    let mut file = std::fs::File::create(dest)
        .map_err(|e| format!("create artifact archive {}: {e}", dest.display()))?;
    let mut total: u64 = 0;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("read artifact body: {}", e.without_url()))?;
        total = total.saturating_add(chunk.len() as u64);
        if total > max_bytes {
            return Err(format!(
                "artifact archive exceeds the {max_bytes}-byte fetch cap (ATO_ARTIFACT_FETCH_MAX_BYTES)"
            ));
        }
        file.write_all(&chunk)
            .map_err(|e| format!("write artifact archive {}: {e}", dest.display()))?;
    }
    file.flush()
        .map_err(|e| format!("flush artifact archive {}: {e}", dest.display()))?;
    Ok(())
}

/// ato#1002: extract a transport `artifact.tar.gz` into `dest_dir`, fail-closed.
///
/// The archive must contain exactly the #928 layout at its root: `manifest.json`,
/// optional `snapshot-manifest-v1.json` + `artifact-envelope-v1.json`, and
/// `cas/*`. Everything else is rejected — absolute paths, `..` traversal,
/// backslashed components, symlinks/hardlinks/devices/fifos, files outside the
/// allowlist — and the summed entry sizes are capped by `max_total_bytes` (a tar
/// entry cannot lie past its header size: the tar layer reads exactly that many
/// bytes). Permissions/mtimes are NOT propagated from the archive — entries are
/// re-created as plain files, another reason `Entry::unpack` is deliberately not
/// used here.
pub(crate) fn safe_extract_artifact_tar_gz(
    tar_gz: &Path,
    dest_dir: &Path,
    max_total_bytes: u64,
) -> std::result::Result<(), String> {
    let file = std::fs::File::open(tar_gz)
        .map_err(|e| format!("open artifact archive {}: {e}", tar_gz.display()))?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    std::fs::create_dir_all(dest_dir)
        .map_err(|e| format!("create extract dir {}: {e}", dest_dir.display()))?;
    let mut total: u64 = 0;
    let mut saw_manifest = false;
    let entries = archive
        .entries()
        .map_err(|e| format!("read artifact archive: {e}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("read artifact archive entry: {e}"))?;
        let raw = entry
            .path()
            .map_err(|e| format!("artifact archive entry path: {e}"))?
            .into_owned();
        let parts = validate_artifact_entry_path(&raw)?;
        match entry.header().entry_type() {
            tar::EntryType::Directory => {
                // Only the root itself ("./") or cas/ subtrees may appear as dirs —
                // a directory named manifest.json would shadow the required file.
                if !(parts.is_empty() || parts[0] == "cas") {
                    return Err(format!(
                        "unexpected directory entry {raw:?} in artifact archive"
                    ));
                }
                let dir = parts.iter().fold(dest_dir.to_path_buf(), |d, p| d.join(p));
                std::fs::create_dir_all(&dir)
                    .map_err(|e| format!("create {}: {e}", dir.display()))?;
            }
            tar::EntryType::Regular => {
                let ok = (parts.len() == 1
                    && matches!(
                        parts[0].as_str(),
                        "manifest.json"
                            | SNAPSHOT_MANIFEST_V1_FILENAME
                            | ARTIFACT_ENVELOPE_V1_FILENAME
                    ))
                    || (parts.len() >= 2 && parts[0] == "cas");
                if !ok {
                    return Err(format!(
                        "artifact archive entry {raw:?} is outside the manifest/sidecar/CAS allowlist"
                    ));
                }
                total = total.saturating_add(entry.size());
                if total > max_total_bytes {
                    return Err(format!(
                        "artifact archive contents exceed the {max_total_bytes}-byte extraction cap \
                         (ATO_ARTIFACT_FETCH_MAX_BYTES)"
                    ));
                }
                let dest = parts.iter().fold(dest_dir.to_path_buf(), |d, p| d.join(p));
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("create {}: {e}", parent.display()))?;
                }
                let mut out = std::fs::File::create(&dest)
                    .map_err(|e| format!("create {}: {e}", dest.display()))?;
                std::io::copy(&mut entry, &mut out)
                    .map_err(|e| format!("extract {}: {e}", dest.display()))?;
                saw_manifest |= parts.len() == 1 && parts[0] == "manifest.json";
            }
            other => {
                return Err(format!(
                    "refusing artifact archive entry {raw:?} of type {other:?} \
                     (regular files and cas/ directories only)"
                ));
            }
        }
    }
    if !saw_manifest {
        return Err("artifact archive carries no manifest.json at its root".to_string());
    }
    Ok(())
}

/// Validate one archive entry path: relative, no `..`/root/prefix components, no
/// backslashes. Returns the clean components (empty = the archive root `./`).
fn validate_artifact_entry_path(raw: &Path) -> std::result::Result<Vec<String>, String> {
    use std::path::Component;
    let mut parts: Vec<String> = Vec::new();
    for comp in raw.components() {
        match comp {
            Component::CurDir => {}
            Component::Normal(os) => {
                let s = os
                    .to_str()
                    .ok_or_else(|| format!("non-UTF-8 entry path {raw:?} in artifact archive"))?;
                if s.contains('\\') {
                    return Err(format!(
                        "backslashed entry path {raw:?} in artifact archive"
                    ));
                }
                parts.push(s.to_string());
            }
            _ => {
                return Err(format!(
                    "unsafe entry path {raw:?} in artifact archive (absolute or traversal)"
                ));
            }
        }
    }
    Ok(parts)
}

/// v1.2 PR 3e: what kind of restore this artifact is, decided fail-closed by
/// [`classify_restore_artifact`]. `Supervisor` carries the binding names read from
/// the sealed manifest — the ONLY source of truth (a lease field is never trusted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RestoreArtifactClass {
    /// The v1 no-binding artifact: restore + expose directly.
    NoBinding,
    /// A supervisor (binding-required) artifact: the runner must resolve + deliver
    /// every named binding over vsock BEFORE exposing traffic. 3e MVP: ALL names
    /// are required — optional-secret semantics do not exist in this path yet.
    Supervisor { binding_names: Vec<String> },
}

/// v1.2 PR 3e: the NARROW supervisor exception to the "no vsock artifact" rule.
/// A BINDING-REQUIRED supervisor restore is allowed ONLY when every one of these
/// holds — anything else fails closed:
/// - the lease kind is `restore_snapshot_with_bindings` (the capability-gated kind);
/// - this runner is opted in (`ATO_RUNNER_SUPERVISOR=1`);
/// - `manifest.has_vsock == true` AND `manifest.supervisor_build` is present
///   (either without the other = an inconsistent artifact, rejected);
/// - every `binding_names` entry parses as a `BindingName`.
///
/// v1.7 (ato#1002 D4): a supervisor artifact with an EMPTY `binding_names` — a
/// Dockerfile import with no secrets, honestly registered as a supervisor build —
/// restores on the ordinary NO-BINDING public lane: the guest-agent is vacuously
/// bound-ready at boot (ato#1001) and there is nothing to deliver, so it takes
/// the plain `restore_snapshot` kind with no supervisor opt-in required, and a
/// `restore_snapshot_with_bindings` lease against it is a kind/artifact mismatch.
///
/// The plain `restore_snapshot` kind otherwise still restores ONLY a no-binding
/// artifact (`!has_vsock`, no supervisor receipt) — the old rejection is
/// unchanged for it. (The backend binding capability is re-checked in the
/// handler, where a backend exists to probe.)
pub(crate) fn classify_restore_artifact(
    manifest: &ReadyStateManifest,
    with_bindings_kind: bool,
    supervisor_enabled: bool,
) -> std::result::Result<RestoreArtifactClass, (String, String)> {
    let err = |m: String| ("artifact_verification_failed".to_string(), m);
    match (&manifest.supervisor_build, manifest.has_vsock) {
        (None, false) => {
            if with_bindings_kind {
                return Err(err(
                    "restore_snapshot_with_bindings lease references a no-binding artifact \
                     (kind/artifact mismatch)"
                        .to_string(),
                ));
            }
            Ok(RestoreArtifactClass::NoBinding)
        }
        (None, true) => Err(err(
            "artifact declares a vsock binding channel but carries no supervisor_build \
             receipt; refusing to restore an inconsistent artifact"
                .to_string(),
        )),
        (Some(_), false) => Err(err(
            "artifact carries a supervisor_build receipt but has_vsock=false; refusing to \
             restore an inconsistent artifact"
                .to_string(),
        )),
        (Some(sup), true) => {
            // v1.7 (ato#1002 D4): zero-binding supervisor artifact = the
            // ordinary no-binding public restore lane (see the doc comment).
            if sup.binding_names.is_empty() {
                if with_bindings_kind {
                    return Err(err(
                        "restore_snapshot_with_bindings lease references a zero-binding \
                         supervisor artifact (nothing to bind) — use the ordinary \
                         restore_snapshot kind (kind/artifact mismatch)"
                            .to_string(),
                    ));
                }
                return Ok(RestoreArtifactClass::NoBinding);
            }
            if !with_bindings_kind {
                return Err(err(
                    "supervisor (binding-required) artifact needs a restore_snapshot_with_bindings \
                     lease; a plain restore_snapshot lease cannot launch it"
                        .to_string(),
                ));
            }
            if !supervisor_enabled {
                return Err(err(
                    "supervisor artifact refused: this runner is not opted into supervisor \
                     restores (set ATO_RUNNER_SUPERVISOR=1)"
                        .to_string(),
                ));
            }
            for name in &sup.binding_names {
                if let Err(e) = protocol::binding_lease::BindingName::parse(name.as_str()) {
                    return Err(err(format!(
                        "supervisor artifact binding name {name:?}: {e}"
                    )));
                }
            }
            Ok(RestoreArtifactClass::Supervisor {
                binding_names: sup.binding_names.clone(),
            })
        }
    }
}

/// Load `manifest.json` and **verify it is exactly the artifact the lease references**,
/// fail-closed. This is the integrity gate `backend.restore` does not provide.
///
/// Checks, in order:
/// - the manifest deserializes;
/// - **`manifest.id() == lease.artifact_manifest_hash`** (recomputed blake3 over the
///   canonical manifest — the artifact-integrity anchor);
/// - `capsule_manifest_hash` / `execution_id` / `snapshot_backend` match the lease;
/// - `runner_class_id` is present and matches the lease (restore also re-gates it, but
///   this gives a clean pre-restore error and pins the lease↔manifest agreement);
/// - the artifact class is admissible for THIS lease kind + runner
///   ([`classify_restore_artifact`]: no-binding for `restore_snapshot`, the narrow
///   supervisor exception for `restore_snapshot_with_bindings`).
pub(crate) fn load_and_verify_manifest(
    manifest_json: &Path,
    cmd: &RestoreSnapshotCommand,
    supervisor_enabled: bool,
) -> std::result::Result<(ReadyStateManifest, RestoreArtifactClass), (String, String)> {
    load_and_verify_manifest_with_surface_capabilities(
        manifest_json,
        cmd,
        supervisor_enabled,
        false,
    )
}

/// Same artifact gate as [`load_and_verify_manifest`], with the runner's live
/// Pixel capability made explicit. The caller must pass true only when the
/// configured gateway and Linux/x86_64 Ready-State path are both operational.
pub(crate) fn load_and_verify_manifest_with_surface_capabilities(
    manifest_json: &Path,
    cmd: &RestoreSnapshotCommand,
    supervisor_enabled: bool,
    pixel_surface_enabled: bool,
) -> std::result::Result<(ReadyStateManifest, RestoreArtifactClass), (String, String)> {
    let err = |m: String| ("artifact_verification_failed".to_string(), m);
    let bytes = std::fs::read(manifest_json)
        .map_err(|e| err(format!("read {}: {e}", manifest_json.display())))?;
    let manifest: ReadyStateManifest =
        serde_json::from_slice(&bytes).map_err(|e| err(format!("parse manifest.json: {e}")))?;

    let recomputed = manifest.id();
    if recomputed != cmd.artifact_manifest_hash {
        return Err(err(format!(
            "artifact_manifest_hash mismatch: lease {} != recomputed {recomputed}",
            cmd.artifact_manifest_hash
        )));
    }
    if manifest.capsule_manifest_hash != cmd.capsule_manifest_hash {
        return Err(err(format!(
            "capsule_manifest_hash mismatch: lease {} != manifest {}",
            cmd.capsule_manifest_hash, manifest.capsule_manifest_hash
        )));
    }
    match manifest.execution_id.as_deref() {
        Some(id) if id == cmd.execution_id => {}
        other => {
            return Err(err(format!(
                "execution_id mismatch: lease {} != manifest {:?}",
                cmd.execution_id, other
            )));
        }
    }
    if manifest.snapshot_backend.kind != cmd.snapshot_backend {
        return Err(err(format!(
            "snapshot_backend mismatch: lease {} != manifest {}",
            cmd.snapshot_backend, manifest.snapshot_backend.kind
        )));
    }
    match manifest.runner_class_id.as_ref().map(|c| c.to_string()) {
        Some(rc) if rc == cmd.runner_class_id => {}
        other => {
            return Err(err(format!(
                "runner_class_id mismatch: lease {} != manifest {:?}",
                cmd.runner_class_id, other
            )));
        }
    }
    verify_surface_negotiation(&manifest, cmd, pixel_surface_enabled).map_err(err)?;
    let class = classify_restore_artifact(&manifest, cmd.with_bindings, supervisor_enabled)?;
    Ok((manifest, class))
}

/// An authenticated Capsule v1 Snapshot sidecar pair: the identity +
/// compatibility manifest and the Artifact Envelope that binds it to the
/// fetched legacy artifact and its acceptance disposition.
#[derive(Debug, Clone)]
pub(crate) struct VerifiedV1Artifact {
    pub manifest: SnapshotManifestV1,
    pub envelope: ArtifactEnvelopeV1,
}

/// For an explicit Capsule v1 restore lease (`cmd.execution_identity_schema ==
/// Some(EXECUTION_CONTRACT_V1_SCHEMA)`), load and fail-closed-verify the fetched
/// `snapshot-manifest-v1.json` + `artifact-envelope-v1.json` sidecar against the
/// ALREADY-verified `legacy` manifest and the lease's own pinned schema/id
/// fields. `Ok(None)` for a legacy lease (no v1 metadata to verify) — the caller
/// then proceeds on `legacy` alone, exactly as before.
///
/// Checks, in order:
/// - the legacy manifest's own `execution_identity_schema` agrees with the lease's;
/// - the legacy manifest's typed v1 `execution_id` (if any) equals the lease's;
/// - the sidecar deserializes and passes [`SnapshotManifestV1::validate`];
/// - the sidecar's `execution_id` equals the lease's;
/// - the sidecar's schema + (derived) `snapshot_id` match the lease's pinned values;
/// - the envelope deserializes and its schema + `envelope_id` match the lease's
///   pinned values;
/// - [`ArtifactEnvelopeV1::verify`] authenticates the envelope against BOTH the
///   legacy manifest and the sidecar — the same boundary a local publication
///   reader uses (see `ready_state::store`).
pub(crate) fn load_verified_v1_artifact(
    paths: &ArtifactPaths,
    legacy: &ReadyStateManifest,
    command: &RestoreSnapshotCommand,
) -> std::result::Result<Option<VerifiedV1Artifact>, (String, String)> {
    let err = |message: String| ("artifact_verification_failed".to_string(), message);
    if command.execution_identity_schema.is_none() {
        return Ok(None);
    }
    if legacy.execution_identity_schema != command.execution_identity_schema {
        return Err(err(
            "lease/legacy manifest execution identity schema mismatch".to_string(),
        ));
    }
    let expected =
        ExecutionId::new(command.execution_id.clone()).map_err(|error| err(error.to_string()))?;
    if legacy.v1_execution_id().map_err(err)?.as_ref() != Some(&expected) {
        return Err(err(
            "lease/legacy manifest Capsule v1 execution_id mismatch".to_string(),
        ));
    }
    let bytes = std::fs::read(&paths.snapshot_manifest_v1_json).map_err(|error| {
        err(format!(
            "read {}: {error}",
            paths.snapshot_manifest_v1_json.display()
        ))
    })?;
    let manifest: SnapshotManifestV1 = serde_json::from_slice(&bytes).map_err(|error| {
        err(format!(
            "parse {}: {error}",
            paths.snapshot_manifest_v1_json.display()
        ))
    })?;
    manifest.validate().map_err(|error| {
        err(format!(
            "validate {}: {error}",
            paths.snapshot_manifest_v1_json.display()
        ))
    })?;
    if manifest.execution_id != expected {
        return Err(err(format!(
            "Snapshot v1 sidecar execution_id mismatch: expected {expected}, found {}",
            manifest.execution_id
        )));
    }
    let snapshot_id = manifest.snapshot_id().map_err(|error| {
        err(format!(
            "derive snapshot_id from {}: {error}",
            paths.snapshot_manifest_v1_json.display()
        ))
    })?;
    if command.snapshot_manifest_schema.as_deref() != Some(manifest.schema.as_str())
        || command.snapshot_manifest_id.as_deref() != Some(snapshot_id.as_str())
    {
        return Err(err(
            "lease/Snapshot v1 manifest schema or id mismatch".to_string()
        ));
    }
    let envelope_bytes = std::fs::read(&paths.artifact_envelope_v1_json).map_err(|error| {
        err(format!(
            "read {}: {error}",
            paths.artifact_envelope_v1_json.display()
        ))
    })?;
    let envelope: ArtifactEnvelopeV1 =
        serde_json::from_slice(&envelope_bytes).map_err(|error| {
            err(format!(
                "parse {}: {error}",
                paths.artifact_envelope_v1_json.display()
            ))
        })?;
    if command.artifact_envelope_schema.as_deref() != Some(envelope.schema.as_str())
        || command.artifact_envelope_id.as_deref() != Some(envelope.envelope_id.as_str())
    {
        return Err(err(
            "lease/Snapshot Artifact Envelope schema or id mismatch".to_string(),
        ));
    }
    envelope
        .verify(legacy, &manifest)
        .map_err(|error| err(error.to_string()))?;
    Ok(Some(VerifiedV1Artifact { manifest, envelope }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd_json(over: serde_json::Value) -> serde_json::Value {
        let mut base = serde_json::json!({
            "kind": "restore_snapshot",
            "snapshot_id": "snap_1",
            "capsule_id": "cap-1",
            "target_label": "web",
            "profile": "default",
            "artifact_location": "cas://job-1/blake3:art",
            "artifact_manifest_hash": "blake3:art",
            "capsule_manifest_hash": "blake3:cap",
            "execution_id": "sha256:exec",
            "runner_class_id": "blake3:rc",
            "snapshot_backend": "firecracker",
            "healthcheck_url_path": "/health",
        });
        if let (Some(b), Some(o)) = (base.as_object_mut(), over.as_object()) {
            for (k, v) in o {
                if v.is_null() {
                    b.remove(k);
                } else {
                    b.insert(k.clone(), v.clone());
                }
            }
        }
        base
    }

    fn explicit_web_surface_command() -> RestoreSnapshotCommand {
        parse_restore_snapshot_command(&cmd_json(serde_json::json!({
            "surface_contract_version": "1",
            "session_id": "session-web-1",
            "accepted_session_surfaces": [{
                "kind": "web",
                "profiles": ["ato.web-surface.v1"]
            }],
            "session_surface": {
                "kind": "web",
                "profile": "ato.web-surface.v1",
                "surface_id": "surface-web-1",
                "embed_policy": "sandboxed"
            }
        })))
        .expect("valid explicit Web surface")
    }

    fn explicit_pixel_surface_command() -> RestoreSnapshotCommand {
        parse_restore_snapshot_command(&cmd_json(serde_json::json!({
            "surface_contract_version": "1",
            "session_id": "session-pixel-1",
            "accepted_session_surfaces": [{
                "kind": "pixel_stream",
                "profiles": ["ato.pixel-stream.v1"]
            }],
            "session_surface": {
                "kind": "pixel_stream",
                "profile": "ato.pixel-stream.v1",
                "surface_id": "surface-pixel-1",
                "transport": "rfb_websocket",
                "viewport": { "width": 1280, "height": 720 },
                "capabilities": {}
            }
        })))
        .expect("valid explicit Pixel surface")
    }

    #[test]
    fn parses_a_full_restore_lease() {
        let c = parse_restore_snapshot_command(&cmd_json(serde_json::json!({}))).unwrap();
        assert_eq!(c.snapshot_id, "snap_1");
        assert_eq!(c.artifact_manifest_hash, "blake3:art");
        assert_eq!(c.healthcheck_url_path.as_deref(), Some("/health"));
        assert!(!c.with_bindings);
        // ato#1002: artifact_fetch_url is OPTIONAL — absent (old leases) parses as None.
        assert!(c.artifact_fetch_url.is_none());
        // Legacy Web leases omit the versioned surface fields entirely.
        assert!(c.surface_contract_version.is_none());
        assert!(c.session_surface.is_none());
        assert!(c.session_id.is_none());
        assert!(c.accepted_session_surfaces.is_none());
        // v1.2 PR 3e: the with-bindings kind parses identically, flagged.
        let c = parse_restore_snapshot_command(&cmd_json(
            serde_json::json!({ "kind": "restore_snapshot_with_bindings" }),
        ))
        .unwrap();
        assert!(c.with_bindings);
        // ato#1002: a present artifact_fetch_url is carried through; blank is None.
        let c = parse_restore_snapshot_command(&cmd_json(
            serde_json::json!({ "artifact_fetch_url": "https://r2.example/presigned?sig=x" }),
        ))
        .unwrap();
        assert_eq!(
            c.artifact_fetch_url.as_deref(),
            Some("https://r2.example/presigned?sig=x")
        );
        let c = parse_restore_snapshot_command(&cmd_json(
            serde_json::json!({ "artifact_fetch_url": "  " }),
        ))
        .unwrap();
        assert!(c.artifact_fetch_url.is_none());
        // P3b: run_id is OPTIONAL/additive — absent (hand-rolled lease) is None;
        // present (every ato-api lease) is carried; blank normalizes to None.
        let c = parse_restore_snapshot_command(&cmd_json(serde_json::json!({}))).unwrap();
        assert!(c.run_id.is_none());
        let c =
            parse_restore_snapshot_command(&cmd_json(serde_json::json!({ "run_id": "01RUNID" })))
                .unwrap();
        assert_eq!(c.run_id.as_deref(), Some("01RUNID"));
        let c = parse_restore_snapshot_command(&cmd_json(serde_json::json!({ "run_id": "  " })))
            .unwrap();
        assert!(c.run_id.is_none());
    }

    #[test]
    fn parses_and_revalidates_an_explicit_pixel_surface_descriptor() {
        let c = parse_restore_snapshot_command(&cmd_json(serde_json::json!({
            "surface_contract_version": "1",
            "session_id": "session-pixel-1",
            "accepted_session_surfaces": [{
                "kind": "pixel_stream",
                "profiles": ["ato.pixel-stream.v1"]
            }],
            "session_surface": {
                "kind": "pixel_stream",
                "profile": "ato.pixel-stream.v1",
                "surface_id": "surface-pixel-1",
                "transport": "rfb_websocket",
                "viewport": { "width": 1280, "height": 720 },
                "capabilities": { "keyboard": "us", "pointer": true }
            }
        })))
        .expect("valid pixel surface");

        assert_eq!(c.surface_contract_version.as_deref(), Some("1"));
        assert_eq!(c.session_id.as_deref(), Some("session-pixel-1"));
        assert!(matches!(
            c.session_surface,
            Some(SessionSurfaceDescriptor::PixelStream { ref surface_id, .. })
                if surface_id == "surface-pixel-1"
        ));
    }

    #[test]
    fn legacy_artifact_accepts_an_explicit_canonical_web_surface() {
        let manifest = manifest_with(None, false);
        let command = explicit_web_surface_command();

        verify_surface_negotiation(&manifest, &command, false)
            .expect("legacy Web artifact must remain compatible with a canonical Web lease");
    }

    #[test]
    fn legacy_artifact_rejects_an_explicit_pixel_surface() {
        let manifest = manifest_with(None, false);
        let command = explicit_pixel_surface_command();

        let error = verify_surface_negotiation(&manifest, &command, true)
            .expect_err("legacy artifact must not inherit a Pixel requirement");

        assert!(error.contains("only an explicit Web surface"), "{error}");
    }

    #[test]
    fn legacy_artifact_rejects_an_unknown_explicit_surface() {
        let manifest = manifest_with(None, false);
        let mut command = explicit_web_surface_command();
        command.session_surface = Some(SessionSurfaceDescriptor::Unknown);

        let error = verify_surface_negotiation(&manifest, &command, false)
            .expect_err("legacy artifact must not inherit an unknown requirement");

        assert!(error.contains("only an explicit Web surface"), "{error}");
    }

    #[test]
    fn explicit_surface_requires_nonempty_client_capabilities() {
        let descriptor = serde_json::json!({
            "kind": "pixel_stream",
            "profile": "ato.pixel-stream.v1",
            "surface_id": "surface-pixel-1",
            "transport": "rfb_websocket",
            "viewport": { "width": 1280, "height": 720 },
            "capabilities": {}
        });
        let missing = parse_restore_snapshot_command(&cmd_json(serde_json::json!({
            "surface_contract_version": "1",
            "session_id": "session-pixel-1",
            "session_surface": descriptor.clone()
        })))
        .expect_err("explicit surface without client capabilities must fail");
        assert!(missing.1.contains("accepted_session_surfaces"));

        let empty = parse_restore_snapshot_command(&cmd_json(serde_json::json!({
            "surface_contract_version": "1",
            "session_id": "session-pixel-1",
            "accepted_session_surfaces": [],
            "session_surface": descriptor
        })))
        .expect_err("empty client capabilities must remain distinct and fail");
        assert!(empty.1.contains("empty"));
    }

    #[test]
    fn explicit_surface_requires_session_binding() {
        let error = parse_restore_snapshot_command(&cmd_json(serde_json::json!({
            "surface_contract_version": "1",
            "accepted_session_surfaces": [{
                "kind": "pixel_stream",
                "profiles": ["ato.pixel-stream.v1"]
            }],
            "session_surface": {
                "kind": "pixel_stream",
                "profile": "ato.pixel-stream.v1",
                "surface_id": "surface-pixel-1",
                "transport": "rfb_websocket",
                "viewport": { "width": 1280, "height": 720 },
                "capabilities": {}
            }
        })))
        .expect_err("explicit surface without session binding must fail");
        assert!(error.1.contains("session_id"), "{}", error.1);
    }

    #[test]
    fn explicit_surface_contract_fields_are_all_or_nothing() {
        let without_version = parse_restore_snapshot_command(&cmd_json(serde_json::json!({
            "session_surface": {
                "kind": "web",
                "profile": "ato.web-surface.v1",
                "surface_id": "surface-web-1",
                "embed_policy": "sandboxed"
            }
        })))
        .expect_err("descriptor without version must fail closed");
        assert_eq!(without_version.0, "invalid_restore_lease");
        assert!(without_version.1.contains("surface_contract_version"));

        let without_descriptor = parse_restore_snapshot_command(&cmd_json(serde_json::json!({
            "surface_contract_version": "1"
        })))
        .expect_err("version without descriptor must fail closed");
        assert_eq!(without_descriptor.0, "invalid_restore_lease");
        assert!(without_descriptor.1.contains("session_surface"));
    }

    #[test]
    fn unknown_or_malformed_explicit_surface_never_falls_back_to_web() {
        let unknown = parse_restore_snapshot_command(&cmd_json(serde_json::json!({
            "surface_contract_version": "1",
            "session_surface": {
                "kind": "future_surface",
                "profile": "ato.future.v1",
                "surface_id": "surface-future-1"
            }
        })))
        .expect_err("unknown descriptor kind must fail");
        assert_eq!(unknown.0, "invalid_restore_lease");
        assert!(unknown.1.contains("unsupported"));

        let wrong_transport = parse_restore_snapshot_command(&cmd_json(serde_json::json!({
            "surface_contract_version": "1",
            "session_surface": {
                "kind": "pixel_stream",
                "profile": "ato.pixel-stream.v1",
                "surface_id": "surface-pixel-1",
                "transport": "https",
                "viewport": { "width": 1280, "height": 720 },
                "capabilities": {}
            }
        })))
        .expect_err("pixel transport must be revalidated on the runner");
        assert_eq!(wrong_transport.0, "invalid_restore_lease");
        assert!(wrong_transport.1.contains("transport"));
    }

    // ── ato#1006 (UNIT C): the preview lease kind + duration fields ────────────

    #[test]
    fn non_preview_leases_carry_no_duration_fields() {
        // A plain restore_snapshot lease (no duration fields on the wire) parses
        // with is_preview=false and both durations None — byte-compatible.
        let c = parse_restore_snapshot_command(&cmd_json(serde_json::json!({}))).unwrap();
        assert!(!c.is_preview);
        assert!(c.max_duration_secs.is_none());
        assert!(c.idle_timeout_secs.is_none());
        // Even if a non-preview lease somehow carried the fields, is_preview stays
        // false (the kind decides), but the values are still parsed additively.
        let c = parse_restore_snapshot_command(&cmd_json(
            serde_json::json!({ "max_duration_secs": 180, "idle_timeout_secs": 45 }),
        ))
        .unwrap();
        assert!(!c.is_preview);
        assert_eq!(c.max_duration_secs, Some(180));
        assert_eq!(c.idle_timeout_secs, Some(45));
    }

    #[test]
    fn preview_lease_parses_durations_and_is_no_binding() {
        let c = parse_restore_snapshot_command(&cmd_json(serde_json::json!({
            "kind": "restore_snapshot_preview",
            "max_duration_secs": 180,
            "idle_timeout_secs": 45,
        })))
        .unwrap();
        assert!(c.is_preview);
        // A preview lease is a NO-BINDING restore — never a supervisor artifact.
        assert!(!c.with_bindings);
        assert_eq!(c.max_duration_secs, Some(180));
        assert_eq!(c.idle_timeout_secs, Some(45));
        // idle_timeout_secs is optional: a preview lease with only the hard cap parses.
        let c = parse_restore_snapshot_command(&cmd_json(serde_json::json!({
            "kind": "restore_snapshot_preview",
            "max_duration_secs": 60,
        })))
        .unwrap();
        assert!(c.is_preview);
        assert_eq!(c.max_duration_secs, Some(60));
        assert!(c.idle_timeout_secs.is_none());
    }

    #[test]
    fn preview_lease_without_max_duration_fails_closed() {
        // Fail closed: a preview must never run without a hard max-duration cap.
        let e = parse_restore_snapshot_command(&cmd_json(serde_json::json!({
            "kind": "restore_snapshot_preview",
            "idle_timeout_secs": 45,
        })))
        .unwrap_err();
        assert_eq!(e.0, "invalid_restore_lease");
        assert!(e.1.contains("max_duration_secs"), "{}", e.1);
    }

    // ── v1.2 PR 3e: the narrow supervisor gate matrix ─────────────────────────

    fn manifest_with(supervisor: Option<Vec<&str>>, has_vsock: bool) -> ReadyStateManifest {
        // Minimal structurally-valid manifest — classify only reads has_vsock +
        // supervisor_build, but build it via serde to stay honest to the schema.
        let mut v = serde_json::json!({
            "schema": "ato.ready-state/v1",
            "capsule_manifest_hash": "blake3:cap",
            "has_vsock": has_vsock,
            "layers": {},
            "snapshot_backend": { "kind": "firecracker", "version": "1", "snapshot_format_version": "fc-v2" },
            "restore_contract": {},
            "sanitizer_contract": { "steps": [] },
        });
        if let Some(names) = supervisor {
            v["supervisor_build"] = serde_json::json!({
                "binding_names": names,
                "page_hygiene_boot_args": true,
            });
        }
        serde_json::from_value(v).expect("manifest")
    }

    #[test]
    fn old_kind_cannot_launch_a_supervisor_artifact() {
        let m = manifest_with(Some(vec!["openai_api_key"]), true);
        // Even with the flag ON: the plain kind must never launch a supervisor artifact.
        let e = classify_restore_artifact(&m, false, true).unwrap_err();
        assert!(e.1.contains("restore_snapshot_with_bindings"), "{}", e.1);
    }

    #[test]
    fn with_bindings_kind_rejects_a_no_binding_artifact() {
        let m = manifest_with(None, false);
        let e = classify_restore_artifact(&m, true, true).unwrap_err();
        assert!(e.1.contains("kind/artifact mismatch"), "{}", e.1);
        // The plain kind still restores it.
        assert_eq!(
            classify_restore_artifact(&m, false, true).unwrap(),
            RestoreArtifactClass::NoBinding
        );
        assert_eq!(
            classify_restore_artifact(&m, false, false).unwrap(),
            RestoreArtifactClass::NoBinding
        );
    }

    #[test]
    fn supervisor_artifact_with_flag_off_is_rejected() {
        let m = manifest_with(Some(vec!["openai_api_key"]), true);
        let e = classify_restore_artifact(&m, true, false).unwrap_err();
        assert!(e.1.contains("ATO_RUNNER_SUPERVISOR"), "{}", e.1);
    }

    #[test]
    fn inconsistent_vsock_supervisor_combinations_are_rejected_both_ways() {
        // has_vsock without a supervisor receipt: the ORIGINAL rejection, kept.
        let m = manifest_with(None, true);
        assert!(
            classify_restore_artifact(&m, false, true)
                .unwrap_err()
                .1
                .contains("no supervisor_build")
        );
        assert!(
            classify_restore_artifact(&m, true, true)
                .unwrap_err()
                .1
                .contains("no supervisor_build")
        );
        // supervisor receipt without vsock: also inconsistent.
        let m = manifest_with(Some(vec!["openai_api_key"]), false);
        assert!(
            classify_restore_artifact(&m, true, true)
                .unwrap_err()
                .1
                .contains("has_vsock=false")
        );
    }

    #[test]
    fn supervisor_binding_names_must_be_valid() {
        // An uppercase (invalid BindingName) name fails closed.
        let m = manifest_with(Some(vec!["OPENAI_API_KEY"]), true);
        assert!(
            classify_restore_artifact(&m, true, true)
                .unwrap_err()
                .1
                .contains("OPENAI_API_KEY")
        );
    }

    // ── v1.7 (ato#1002 D4): zero-binding supervisor artifact = the no-binding lane ──

    #[test]
    fn zero_binding_supervisor_artifact_restores_on_the_plain_no_binding_lane() {
        // A Dockerfile import with no secrets: supervisor_build present +
        // has_vsock, EMPTY binding_names. Ordinary restore_snapshot kind,
        // NO supervisor opt-in required (guest is vacuously bound-ready,
        // ato#1001) — this is the artifact PR D's managed restore serves.
        let m = manifest_with(Some(vec![]), true);
        assert_eq!(
            classify_restore_artifact(&m, false, false).unwrap(),
            RestoreArtifactClass::NoBinding
        );
        assert_eq!(
            classify_restore_artifact(&m, false, true).unwrap(),
            RestoreArtifactClass::NoBinding
        );
    }

    #[test]
    fn zero_binding_supervisor_artifact_rejects_the_with_bindings_kind() {
        let m = manifest_with(Some(vec![]), true);
        let e = classify_restore_artifact(&m, true, true).unwrap_err();
        assert!(e.1.contains("kind/artifact mismatch"), "{}", e.1);
        assert!(e.1.contains("restore_snapshot"), "{}", e.1);
        // supervisor_enabled makes no difference — nothing to bind either way.
        let e = classify_restore_artifact(&m, true, false).unwrap_err();
        assert!(e.1.contains("kind/artifact mismatch"), "{}", e.1);
    }

    #[test]
    fn non_empty_supervisor_artifact_still_requires_the_with_bindings_kind() {
        // Invariance: the zero-binding lane must not have loosened the
        // binding-required gate (reviewer matrix cases 3 and 4).
        let m = manifest_with(Some(vec!["openai_api_key"]), true);
        let e = classify_restore_artifact(&m, false, true).unwrap_err();
        assert!(e.1.contains("restore_snapshot_with_bindings"), "{}", e.1);
        assert_eq!(
            classify_restore_artifact(&m, true, true).unwrap(),
            RestoreArtifactClass::Supervisor {
                binding_names: vec!["openai_api_key".into()]
            }
        );
    }

    #[test]
    fn supervisor_artifact_with_every_prerequisite_classifies_with_manifest_names() {
        let m = manifest_with(Some(vec!["openai_api_key", "db_url"]), true);
        let class = classify_restore_artifact(&m, true, true).unwrap();
        assert_eq!(
            class,
            RestoreArtifactClass::Supervisor {
                binding_names: vec!["openai_api_key".into(), "db_url".into()]
            }
        );
    }

    #[test]
    fn rejects_wrong_kind_and_missing_fields() {
        assert!(
            parse_restore_snapshot_command(&serde_json::json!({ "kind": "run_capsule" })).is_err()
        );
        for field in [
            "snapshot_id",
            "capsule_id",
            "target_label",
            "profile",
            "artifact_location",
            "artifact_manifest_hash",
            "capsule_manifest_hash",
            "execution_id",
            "runner_class_id",
            "snapshot_backend",
        ] {
            let c = cmd_json(serde_json::json!({ field: serde_json::Value::Null }));
            let e = parse_restore_snapshot_command(&c).unwrap_err();
            assert!(
                e.1.contains(field),
                "missing {field} should be reported: {}",
                e.1
            );
        }
        // healthcheck is optional.
        assert!(
            parse_restore_snapshot_command(&cmd_json(
                serde_json::json!({ "healthcheck_url_path": serde_json::Value::Null })
            ))
            .unwrap()
            .healthcheck_url_path
            .is_none()
        );
    }

    #[test]
    fn locate_artifact_maps_cas_uri_and_rejects_escapes() {
        let root = Path::new("/var/lib/ato/artifacts");
        let p = locate_artifact("cas://job-1/blake3:art", root).unwrap();
        assert_eq!(p.manifest_json, root.join("job-1").join("manifest.json"));
        assert_eq!(
            p.snapshot_manifest_v1_json,
            root.join("job-1").join("snapshot-manifest-v1.json")
        );
        assert_eq!(p.cas_dir, root.join("job-1").join("cas"));
        // Unsupported schemes (a bare https:// location stays rejected).
        assert!(
            locate_artifact("https://evil/x", root)
                .unwrap_err()
                .1
                .contains("scheme")
        );
        assert!(
            locate_artifact("s3://bucket/job/x", root)
                .unwrap_err()
                .1
                .contains("scheme")
        );
        // Traversal / absolute / multi-segment job.
        assert!(locate_artifact("cas://../etc/x", root).is_err());
        assert!(locate_artifact("cas:///abs/x", root).is_err());
        // "." is a CurDir component, NOT Normal — it would resolve the job dir to
        // artifact_root itself.
        assert!(locate_artifact("cas://./x", root).is_err());
        // Windows drive prefix is a Prefix component, NOT Normal — `root.join("C:")`
        // would escape artifact_root entirely.
        #[cfg(windows)]
        assert!(locate_artifact("cas://C:/x", root).is_err());
    }

    #[test]
    fn locate_artifact_maps_r2_uri_and_rejects_escapes() {
        // ato#1002: r2://<bucket>/<job_id>/<hash> maps to the SAME local layout —
        // the bucket never shapes the path.
        let root = Path::new("/var/lib/ato/artifacts");
        let p = locate_artifact("r2://ato-artifacts/job-9/blake3:art", root).unwrap();
        assert_eq!(p.manifest_json, root.join("job-9").join("manifest.json"));
        assert_eq!(
            p.snapshot_manifest_v1_json,
            root.join("job-9").join("snapshot-manifest-v1.json")
        );
        assert_eq!(p.cas_dir, root.join("job-9").join("cas"));
        // Missing job / missing bucket / traversal / absolute job.
        assert!(locate_artifact("r2://bucket", root).is_err());
        assert!(
            locate_artifact("r2:///job/x", root)
                .unwrap_err()
                .1
                .contains("bucket")
        );
        assert!(locate_artifact("r2://bucket/../x", root).is_err());
        assert!(locate_artifact("r2://bucket//abs", root).is_err());
        // "." job segment: without the Component::Normal requirement this resolved
        // the job dir to artifact_root itself, and ensure_artifact_local's
        // pre-publish cleanup would remove_dir_all the ENTIRE artifact root.
        assert!(locate_artifact("r2://bucket/./x", root).is_err());
        #[cfg(windows)]
        assert!(locate_artifact("r2://bucket/C:/x", root).is_err());
    }

    #[test]
    fn verify_rejects_a_tampered_or_binding_manifest() {
        // Build a real sealed manifest via the Fake backend, persist it, and verify.
        use capsulefs::CasStore;
        use snapshot::{
            BuildLayers, BuildReadyStateInput, FakeSnapshotBackend, RestoreContract,
            SanitizerContract, SnapshotBackend,
        };
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path().join("cas")).unwrap();
        let receipt = FakeSnapshotBackend::new()
            .build_ready_state(BuildReadyStateInput {
                store: &store,
                capsule_manifest_hash: "blake3:cap".into(),
                runner_class: Some(
                    capsule::foundation::install_lifecycle::RunnerClassFacts::from_host().id(),
                ),
                surface_requirement: None,
                layers: BuildLayers {
                    rootfs: b"rootfs".to_vec(),
                    runtime: None,
                    dependency: None,
                    app: None,
                    vmstate: vec![1u8; 64],
                    memory: vec![2u8; 4096],
                },
                restore_contract: RestoreContract {
                    ports: vec![8080],
                    healthcheck: Some("/health".into()),
                    expected_ready_ms: Some(2000),
                    ..Default::default()
                },
                sanitizer_contract: SanitizerContract::default(),
                declared_secret_markers: vec![],
                execution_id: Some("sha256:exec".into()),
                supervisor: None,
            })
            .expect("build");
        let m = receipt.manifest;
        let mpath = dir.path().join("manifest.json");
        std::fs::write(&mpath, serde_json::to_vec(&m).unwrap()).unwrap();
        let rc = m.runner_class_id.as_ref().unwrap().to_string();

        let base = RestoreSnapshotCommand {
            snapshot_id: "snap_1".into(),
            capsule_id: "cap-1".into(),
            target_label: "web".into(),
            profile: "default".into(),
            artifact_location: "cas://job/blake3".into(),
            artifact_fetch_url: None,
            artifact_manifest_hash: m.id(),
            capsule_manifest_hash: "blake3:cap".into(),
            execution_id: "sha256:exec".into(),
            execution_identity_schema: None,
            snapshot_manifest_schema: None,
            snapshot_manifest_id: None,
            artifact_envelope_schema: None,
            artifact_envelope_id: None,
            runner_class_id: rc.clone(),
            snapshot_backend: m.snapshot_backend.kind.clone(),
            healthcheck_url_path: Some("/health".into()),
            session_surface: None,
            surface_contract_version: None,
            session_id: None,
            accepted_session_surfaces: None,
            with_bindings: false,
            is_preview: false,
            max_duration_secs: None,
            idle_timeout_secs: None,
            run_id: None,
        };
        // Exact match ⇒ ok, classified NoBinding.
        let (_, class) = load_and_verify_manifest(&mpath, &base, false).unwrap();
        assert_eq!(class, RestoreArtifactClass::NoBinding);
        // Tampered artifact hash ⇒ fail (the integrity anchor restore() lacks).
        let mut bad = base.clone();
        bad.artifact_manifest_hash = "blake3:TAMPERED".into();
        assert!(
            load_and_verify_manifest(&mpath, &bad, false)
                .unwrap_err()
                .1
                .contains("artifact_manifest_hash mismatch")
        );
        // Wrong execution_id / capsule hash / runner class / backend ⇒ fail.
        for mutate in [
            |c: &mut RestoreSnapshotCommand| c.execution_id = "sha256:other".into(),
            |c: &mut RestoreSnapshotCommand| c.capsule_manifest_hash = "blake3:other".into(),
            |c: &mut RestoreSnapshotCommand| c.runner_class_id = "blake3:other".into(),
            |c: &mut RestoreSnapshotCommand| c.snapshot_backend = "qemu".into(),
        ] {
            let mut c = base.clone();
            mutate(&mut c);
            assert!(load_and_verify_manifest(&mpath, &c, false).is_err());
        }

        // Explicit surfaces are re-negotiated from artifact × launch client ×
        // this runner, then compared with the descriptor selected by the API.
        let mut pixel_manifest = m.clone();
        pixel_manifest.surface_requirement =
            Some(protocol::session_surface::SessionSurfaceRequirement {
                kind: SessionSurfaceKind::PixelStream,
                profiles: Some(vec![PIXEL_STREAM_PROFILE.to_string()]),
            });
        std::fs::write(
            &mpath,
            serde_json::to_vec(&pixel_manifest).expect("serialize pixel manifest"),
        )
        .unwrap();
        let mut pixel = base.clone();
        pixel.artifact_manifest_hash = pixel_manifest.id();
        pixel.surface_contract_version = Some(SESSION_SURFACE_CONTRACT_VERSION.to_string());
        pixel.session_id = Some("session-pixel-1".to_string());
        pixel.accepted_session_surfaces = Some(vec![AcceptedSessionSurface {
            kind: SessionSurfaceKind::PixelStream,
            profiles: Some(vec![PIXEL_STREAM_PROFILE.to_string()]),
        }]);
        pixel.session_surface = Some(SessionSurfaceDescriptor::PixelStream {
            profile: PIXEL_STREAM_PROFILE.to_string(),
            surface_id: "surface-pixel-1".to_string(),
            transport: SessionSurfaceTransport::RfbWebsocket,
            viewport: protocol::session_surface::PixelStreamViewport {
                width: 1280,
                height: 720,
            },
            capabilities: Default::default(),
        });
        let disabled =
            load_and_verify_manifest_with_surface_capabilities(&mpath, &pixel, false, false)
                .expect_err("disabled local gateway must not advertise Pixel");
        assert!(disabled.1.contains("runner"), "{}", disabled.1);
        load_and_verify_manifest_with_surface_capabilities(&mpath, &pixel, false, true)
            .expect("matching explicit pixel negotiation");

        let mut client_mismatch = pixel.clone();
        client_mismatch.accepted_session_surfaces = Some(vec![AcceptedSessionSurface {
            kind: SessionSurfaceKind::Web,
            profiles: Some(vec![WEB_SURFACE_PROFILE.to_string()]),
        }]);
        let mismatch = load_and_verify_manifest_with_surface_capabilities(
            &mpath,
            &client_mismatch,
            false,
            true,
        )
        .expect_err("client mismatch must fail before restore");
        assert!(mismatch.1.contains("renegotiation"), "{}", mismatch.1);

        let mut omitted = pixel;
        omitted.session_surface = None;
        omitted.surface_contract_version = None;
        omitted.session_id = None;
        omitted.accepted_session_surfaces = None;
        let missing =
            load_and_verify_manifest_with_surface_capabilities(&mpath, &omitted, false, true)
                .expect_err("pixel artifact must never use legacy Web omission");
        assert!(
            missing.1.contains("omitted session_surface"),
            "{}",
            missing.1
        );
    }

    // ── Capsule v1: explicit-schema lease completeness + envelope authentication ──

    #[test]
    fn explicit_v1_schema_requires_complete_snapshot_metadata() {
        let error = parse_restore_snapshot_command(&cmd_json(serde_json::json!({
            "execution_id": format!("blake3:{}", "a".repeat(64)),
            "execution_identity_schema": EXECUTION_CONTRACT_V1_SCHEMA,
        })))
        .unwrap_err();

        assert!(error.1.contains("requires snapshot manifest/envelope"));
    }

    #[test]
    fn legacy_blake3_execution_id_does_not_imply_v1_schema() {
        let command = parse_restore_snapshot_command(&cmd_json(serde_json::json!({
            "execution_id": format!("blake3:{}", "b".repeat(64)),
        })))
        .unwrap();

        assert!(command.execution_identity_schema.is_none());
        assert!(command.snapshot_manifest_schema.is_none());
    }

    #[test]
    fn authenticated_v1_envelope_rejects_a_recomputed_tampered_sidecar() {
        use capsule::execution_contract::ExecutionId;
        use capsule::snapshot_manifest::{
            CapturePolicyV1, PortabilityTier, RestoreContractV1, SNAPSHOT_COMPATIBILITY_V1_SCHEMA,
            SNAPSHOT_RESTORE_CONTRACT_V1_SCHEMA, SNAPSHOT_SANITIZATION_ATTESTATION_V1_SCHEMA,
            SNAPSHOT_SECRET_SCAN_ATTESTATION_V1_SCHEMA, SanitizationAttestationV1,
            SecretScanAttestationV1, SnapshotBackendKind, SnapshotCaptureProvenance,
            SnapshotCompatibilityContractV1,
        };
        use capsulefs::CasStore;
        use snapshot::{
            BuildLayers, BuildReadyStateInput, FakeSnapshotBackend, RestoreContract,
            SanitizerContract, SnapshotBackend,
        };

        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path().join("cas")).unwrap();
        let backend = FakeSnapshotBackend::new();
        let execution_id = ExecutionId::new(format!("blake3:{}", "a".repeat(64))).unwrap();
        let legacy = backend
            .build_ready_state(BuildReadyStateInput {
                store: &store,
                capsule_manifest_hash: format!("blake3:{}", "c".repeat(64)),
                runner_class: Some(
                    capsule::foundation::install_lifecycle::RunnerClassFacts::from_host().id(),
                ),
                surface_requirement: None,
                layers: BuildLayers {
                    rootfs: b"rootfs".to_vec(),
                    runtime: None,
                    dependency: None,
                    app: None,
                    vmstate: vec![1; 64],
                    memory: vec![2; 4096],
                },
                restore_contract: RestoreContract::default(),
                sanitizer_contract: SanitizerContract::default(),
                declared_secret_markers: Vec::new(),
                execution_id: Some(execution_id.to_string()),
                supervisor: None,
            })
            .unwrap()
            .manifest;
        // The legacy artifact's own `execution_identity_schema` is what
        // `load_verified_v1_artifact` cross-checks against the lease — a real
        // v1-sealed artifact carries it (a fast follow to the Fake backend's
        // `build_ready_state`, which does not yet accept the schema as input).
        let mut legacy = legacy;
        legacy.execution_identity_schema =
            Some(capsule::execution_contract::EXECUTION_CONTRACT_V1_SCHEMA.to_string());

        let digest = |fill: char| {
            capsule::execution_contract::ContentDigest::try_from(format!(
                "blake3:{}",
                fill.to_string().repeat(64)
            ))
            .unwrap()
        };
        // Constructed directly (not via a backend's `snapshot_compatibility_contract`)
        // since this test only needs a self-consistent v1 sidecar to authenticate —
        // the envelope-boundary behavior under test does not depend on which
        // backend's real facts populate it.
        let sidecar = SnapshotManifestV1 {
            schema: SNAPSHOT_MANIFEST_V1_SCHEMA.to_string(),
            execution_id: execution_id.clone(),
            compatibility_contract: SnapshotCompatibilityContractV1 {
                schema: SNAPSHOT_COMPATIBILITY_V1_SCHEMA.to_string(),
                backend: SnapshotBackendKind::Fake,
                format_version: 1,
                vmm_identity: "fake-0.1.0".to_string(),
                state_codec: "raw".to_string(),
                guest_kernel_identity: "none:fake-backend".to_string(),
                cpu_template: "none".to_string(),
                // Must equal `restore_contract.restore_protocol` below —
                // `SnapshotManifestV1::validate` enforces they are the SAME
                // restore protocol identity.
                runner_restore_contract: "ato-restore/v1".to_string(),
                portability_tier: PortabilityTier::ClassPortable,
                compatibility_class_identity: digest('c'),
            },
            memory_layer_refs: vec![digest('1')],
            vmstate_layer_refs: vec![digest('2')],
            disk_layer_refs: vec![digest('3')],
            restore_contract: RestoreContractV1 {
                schema: SNAPSHOT_RESTORE_CONTRACT_V1_SCHEMA.to_string(),
                restore_protocol: "ato-restore/v1".to_string(),
                steps: Vec::new(),
            },
            capture_policy: CapturePolicyV1::Running,
            capture_provenance: SnapshotCaptureProvenance::default(),
            sanitization_attestation: SanitizationAttestationV1 {
                schema: SNAPSHOT_SANITIZATION_ATTESTATION_V1_SCHEMA.to_string(),
                steps: Vec::new(),
            },
            secret_scan_attestation: SecretScanAttestationV1 {
                schema: SNAPSHOT_SECRET_SCAN_ATTESTATION_V1_SCHEMA.to_string(),
                scanner_identity: "ato-secret-scan/1.0".to_string(),
                policy_identity: "default/v1".to_string(),
                scanned_layers: Vec::new(),
                verdict: "clean".to_string(),
            },
        };
        let envelope = ArtifactEnvelopeV1::accepted(&legacy, &sidecar).unwrap();
        let snapshot_id = sidecar.snapshot_id().unwrap();
        let paths = ArtifactPaths {
            manifest_json: dir.path().join("manifest.json"),
            snapshot_manifest_v1_json: dir.path().join(SNAPSHOT_MANIFEST_V1_FILENAME),
            artifact_envelope_v1_json: dir.path().join(ARTIFACT_ENVELOPE_V1_FILENAME),
            cas_dir: dir.path().join("cas"),
        };
        std::fs::write(&paths.manifest_json, serde_json::to_vec(&legacy).unwrap()).unwrap();
        std::fs::write(
            &paths.snapshot_manifest_v1_json,
            serde_json::to_vec(&sidecar).unwrap(),
        )
        .unwrap();
        std::fs::write(
            &paths.artifact_envelope_v1_json,
            serde_json::to_vec(&envelope).unwrap(),
        )
        .unwrap();
        let command = parse_restore_snapshot_command(&cmd_json(serde_json::json!({
            "snapshot_id": snapshot_id.as_str(),
            "artifact_manifest_hash": legacy.id(),
            "capsule_manifest_hash": legacy.capsule_manifest_hash,
            "execution_id": execution_id.to_string(),
            "execution_identity_schema": EXECUTION_CONTRACT_V1_SCHEMA,
            "snapshot_manifest_schema": SNAPSHOT_MANIFEST_V1_SCHEMA,
            "snapshot_manifest_id": snapshot_id.as_str(),
            "artifact_envelope_schema": ARTIFACT_ENVELOPE_V1_SCHEMA,
            "artifact_envelope_id": envelope.envelope_id,
            "runner_class_id": legacy.runner_class_id.as_ref().unwrap().to_string(),
            "snapshot_backend": legacy.snapshot_backend.kind,
        })))
        .unwrap();

        load_verified_v1_artifact(&paths, &legacy, &command)
            .unwrap()
            .expect("authenticated v1 artifact");

        let mut tampered = sidecar;
        tampered
            .sanitization_attestation
            .steps
            .push("attacker-controlled".to_string());
        std::fs::write(
            &paths.snapshot_manifest_v1_json,
            serde_json::to_vec(&tampered).unwrap(),
        )
        .unwrap();

        let error = load_verified_v1_artifact(&paths, &legacy, &command).unwrap_err();
        assert!(
            error.1.contains("manifest schema or id mismatch")
                || error.1.contains("does not authenticate"),
            "{}",
            error.1
        );
    }

    // ── ato#1002: safe transport-archive extraction + remote fetch ───────────

    /// Build an `artifact.tar.gz` through the NORMAL Builder API (which validates
    /// paths — hostile shapes are crafted via [`append_raw`] instead).
    fn artifact_targz(files: &[(&str, &[u8])], dirs: &[&str]) -> Vec<u8> {
        let enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut b = tar::Builder::new(enc);
        for d in dirs {
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(tar::EntryType::Directory);
            h.set_size(0);
            h.set_mode(0o755);
            b.append_data(&mut h, *d, std::io::empty()).unwrap();
        }
        for (path, bytes) in files {
            let mut h = tar::Header::new_gnu();
            h.set_size(bytes.len() as u64);
            h.set_mode(0o644);
            b.append_data(&mut h, *path, *bytes).unwrap();
        }
        b.into_inner().unwrap().finish().unwrap()
    }

    /// Append a RAW entry, bypassing Builder path validation — models a hostile
    /// archive (absolute/traversal names, forbidden entry types).
    fn append_raw<W: std::io::Write>(
        b: &mut tar::Builder<W>,
        name: &[u8],
        ty: tar::EntryType,
        data: &[u8],
    ) {
        let mut h = tar::Header::new_gnu();
        h.as_gnu_mut().unwrap().name[..name.len()].copy_from_slice(name);
        h.set_entry_type(ty);
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        b.append(&h, data).unwrap();
    }

    fn hostile_targz(
        build: impl FnOnce(&mut tar::Builder<flate2::write::GzEncoder<Vec<u8>>>),
    ) -> Vec<u8> {
        let enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut b = tar::Builder::new(enc);
        build(&mut b);
        b.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn safe_extract_accepts_the_canonical_artifact_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let gz = tmp.path().join("artifact.tar.gz");
        std::fs::write(
            &gz,
            artifact_targz(
                &[
                    ("manifest.json", br#"{"k":1}"#),
                    ("snapshot-manifest-v1.json", br#"{"schema":"v1"}"#),
                    ("artifact-envelope-v1.json", br#"{"schema":"v1"}"#),
                    ("cas/ab/cdef", b"blob"),
                ],
                &["cas/", "cas/ab/"],
            ),
        )
        .unwrap();
        let dest = tmp.path().join("out");
        safe_extract_artifact_tar_gz(&gz, &dest, 8 * 1024).unwrap();
        assert_eq!(
            std::fs::read(dest.join("manifest.json")).unwrap(),
            br#"{"k":1}"#
        );
        assert_eq!(
            std::fs::read(dest.join("snapshot-manifest-v1.json")).unwrap(),
            br#"{"schema":"v1"}"#
        );
        assert_eq!(
            std::fs::read(dest.join("artifact-envelope-v1.json")).unwrap(),
            br#"{"schema":"v1"}"#
        );
        assert_eq!(
            std::fs::read(dest.join("cas").join("ab").join("cdef")).unwrap(),
            b"blob"
        );
    }

    #[test]
    fn safe_extract_rejects_traversal_absolute_and_type_escapes() {
        let tmp = tempfile::tempdir().unwrap();
        let case = |name: &str, bytes: Vec<u8>| {
            let gz = tmp.path().join(name);
            std::fs::write(&gz, bytes).unwrap();
            safe_extract_artifact_tar_gz(&gz, &tmp.path().join(format!("{name}.out")), 1024)
                .unwrap_err()
        };
        // '..' traversal.
        let e = case(
            "trav.tar.gz",
            hostile_targz(|b| append_raw(b, b"../evil", tar::EntryType::Regular, b"boom")),
        );
        assert!(e.contains("unsafe entry path"), "{e}");
        // Absolute path.
        let e = case(
            "abs.tar.gz",
            hostile_targz(|b| append_raw(b, b"/abs/evil", tar::EntryType::Regular, b"boom")),
        );
        assert!(e.contains("unsafe entry path"), "{e}");
        // Regular file outside the manifest/sidecar/CAS allowlist.
        let e = case(
            "root.tar.gz",
            artifact_targz(&[("manifest.json", b"{}"), ("evil.sh", b"#!")], &[]),
        );
        assert!(
            e.contains("outside the manifest/sidecar/CAS allowlist"),
            "{e}"
        );
        // Unexpected directory outside cas/.
        let e = case(
            "dir.tar.gz",
            hostile_targz(|b| append_raw(b, b"weird", tar::EntryType::Directory, b"")),
        );
        assert!(e.contains("unexpected directory"), "{e}");
        // Symlink entry.
        let e = case(
            "link.tar.gz",
            hostile_targz(|b| {
                let mut h = tar::Header::new_gnu();
                h.set_entry_type(tar::EntryType::Symlink);
                h.set_size(0);
                b.append_link(&mut h, "cas/evil-link", "target").unwrap();
            }),
        );
        assert!(e.contains("refusing artifact archive entry"), "{e}");
        // Hardlink entry.
        let e = case(
            "hard.tar.gz",
            hostile_targz(|b| {
                let mut h = tar::Header::new_gnu();
                h.set_entry_type(tar::EntryType::Link);
                h.set_size(0);
                b.append_link(&mut h, "cas/evil-hard", "manifest.json")
                    .unwrap();
            }),
        );
        assert!(e.contains("refusing artifact archive entry"), "{e}");
    }

    #[test]
    fn safe_extract_enforces_the_size_cap_and_requires_a_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        // Summed entry sizes above the cap are refused mid-walk.
        let gz = tmp.path().join("big.tar.gz");
        std::fs::write(
            &gz,
            artifact_targz(&[("manifest.json", b"{}"), ("cas/big", &[0u8; 64])], &[]),
        )
        .unwrap();
        let e = safe_extract_artifact_tar_gz(&gz, &tmp.path().join("o1"), 32).unwrap_err();
        assert!(e.contains("extraction cap"), "{e}");
        // An archive with no root manifest.json is refused.
        let gz = tmp.path().join("nomanifest.tar.gz");
        std::fs::write(&gz, artifact_targz(&[("cas/only", b"x")], &[])).unwrap();
        let e = safe_extract_artifact_tar_gz(&gz, &tmp.path().join("o2"), 1024).unwrap_err();
        assert!(e.contains("no manifest.json"), "{e}");
    }

    /// Minimal localhost HTTP/1.1 fixture: serves `body` with `status` to every
    /// connection for the test's lifetime (thread parks on accept; dies with the
    /// process). The URL path mirrors the R2 object key shape
    /// `<job_id>/<artifact_manifest_hash>/artifact.tar.gz` (ato#1002) — the runner
    /// treats the presigned URL as opaque, so the path is representative only.
    fn spawn_http_fixture(status: &'static str, body: Vec<u8>) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { break };
                let mut head = Vec::new();
                let mut buf = [0u8; 4096];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match s.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => head.extend_from_slice(&buf[..n]),
                    }
                    if head.len() > 64 * 1024 {
                        break;
                    }
                }
                let _ = write!(
                    s,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(&body);
                let _ = s.flush();
            }
        });
        format!("http://{addr}/job/blake3:art/artifact.tar.gz?sig=presigned-fixture")
    }

    #[tokio::test]
    async fn ensure_artifact_local_cas_is_a_pure_passthrough() {
        let tmp = tempfile::tempdir().unwrap();
        let client = reqwest::Client::new();
        // Nothing on disk, no URL: the cas:// mapping comes back untouched
        // (existence is the verify gate's problem, exactly as before).
        let p = ensure_artifact_local(&client, "cas://job-1/blake3:art", tmp.path(), None, 1024)
            .await
            .unwrap();
        assert_eq!(
            p.manifest_json,
            tmp.path().join("job-1").join("manifest.json")
        );
        assert!(!p.manifest_json.exists());
    }

    #[tokio::test]
    async fn ensure_artifact_local_r2_uses_a_local_copy_without_a_url() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("job-1").join("cas")).unwrap();
        std::fs::write(tmp.path().join("job-1").join("manifest.json"), b"{}").unwrap();
        let client = reqwest::Client::new();
        let p = ensure_artifact_local(
            &client,
            "r2://bucket/job-1/blake3:art",
            tmp.path(),
            None,
            1024,
        )
        .await
        .unwrap();
        assert!(p.manifest_json.exists());
    }

    #[tokio::test]
    async fn ensure_artifact_local_r2_without_url_or_local_copy_is_a_clear_error() {
        let tmp = tempfile::tempdir().unwrap();
        let client = reqwest::Client::new();
        let (code, msg) = ensure_artifact_local(
            &client,
            "r2://bucket/job-1/blake3:art",
            tmp.path(),
            None,
            1024,
        )
        .await
        .unwrap_err();
        assert_eq!(code, "artifact_unavailable");
        assert!(msg.contains("artifact_fetch_url"), "{msg}");
    }

    #[tokio::test]
    async fn ensure_artifact_local_r2_downloads_extracts_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let body = artifact_targz(
            &[("manifest.json", br#"{"k":1}"#), ("cas/aa/blob", b"bytes")],
            &["cas/"],
        );
        let url = spawn_http_fixture("200 OK", body);
        let client = reqwest::Client::new();
        let p = ensure_artifact_local(
            &client,
            "r2://bucket/job-7/blake3:art",
            tmp.path(),
            Some(&url),
            1 << 20,
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&p.manifest_json).unwrap(), br#"{"k":1}"#);
        assert_eq!(
            std::fs::read(p.cas_dir.join("aa").join("blob")).unwrap(),
            b"bytes"
        );
        // Atomic publish left no staging temp files beside the job dir.
        let names: Vec<String> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, vec!["job-7".to_string()]);
        // Second call: idempotent local hit, no URL required.
        let p2 = ensure_artifact_local(
            &client,
            "r2://bucket/job-7/blake3:art",
            tmp.path(),
            None,
            1 << 20,
        )
        .await
        .unwrap();
        assert_eq!(p, p2);
    }

    #[tokio::test]
    async fn ensure_artifact_local_r2_rejects_a_bad_archive_and_leaves_no_partial_state() {
        let tmp = tempfile::tempdir().unwrap();
        // No root manifest.json → the extractor refuses; the job dir must NOT appear.
        let url = spawn_http_fixture("200 OK", artifact_targz(&[("cas/only", b"x")], &[]));
        let client = reqwest::Client::new();
        let (code, msg) = ensure_artifact_local(
            &client,
            "r2://bucket/job-3/x",
            tmp.path(),
            Some(&url),
            1 << 20,
        )
        .await
        .unwrap_err();
        assert_eq!(code, "artifact_unavailable");
        assert!(msg.contains("manifest.json"), "{msg}");
        assert!(!tmp.path().join("job-3").exists());
    }

    #[tokio::test]
    async fn ensure_artifact_local_r2_enforces_the_download_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let url = spawn_http_fixture("200 OK", vec![0u8; 4096]);
        let client = reqwest::Client::new();
        let (_, msg) =
            ensure_artifact_local(&client, "r2://bucket/job-5/x", tmp.path(), Some(&url), 1024)
                .await
                .unwrap_err();
        assert!(msg.contains("fetch cap"), "{msg}");
    }

    #[tokio::test]
    async fn ensure_artifact_local_r2_surfaces_http_failures_without_the_url() {
        let tmp = tempfile::tempdir().unwrap();
        let url = spawn_http_fixture("404 Not Found", Vec::new());
        let client = reqwest::Client::new();
        let (_, msg) =
            ensure_artifact_local(&client, "r2://bucket/job-4/x", tmp.path(), Some(&url), 1024)
                .await
                .unwrap_err();
        assert!(msg.contains("HTTP 404"), "{msg}");
        // The presigned URL (query = authorization) must never leak into errors.
        assert!(
            !msg.contains("127.0.0.1") && !msg.contains("sig="),
            "URL leaked: {msg}"
        );
    }
}
