//! v1.7 Dockerfile-to-Snapshot Import (ato#994) — build-tool probe + Dockerfile
//! build execution. **Build/import time only**: everything here runs on the
//! builder host; nothing it produces puts a container runtime inside a restored
//! VM. Rootfs export + guest-agent injection land in the next slice.
//!
//! Command execution goes through [`ImportCommandRunner`] so every flow is
//! unit-testable with a scripted fake — the same seam shape as the cli crate's
//! `OciCommandRunner` (`adapters/runtime/oci_provider.rs`), which cannot be
//! reused directly today: it is `pub(crate)` to cli, tokio-oriented, and models
//! container RUN operations (create/start/logs), not image builds. Consolidating
//! the two seams is a noted follow-up, not a v0 requirement.
//!
//! Tool policy (builder requirements in ato#994): the import drives a
//! **docker-CLI-compatible** tool — `podman` preferred (rootless lane), `docker`
//! accepted (the existing `rootfs_builder` build script already requires it on
//! builder hosts). `buildah` is detected and reported but not driven in v0: its
//! build/inspect CLI diverges, and podman wraps buildah anyway.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::{BuildTool, DOCKER_IMPORT_PLATFORM, DockerImportSpec, ResolvedBaseImage};

/// Output of one builder-host command. `status` is the raw exit code.
#[derive(Debug, Clone)]
pub struct ImportCommandOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl ImportCommandOutput {
    fn success(&self) -> bool {
        self.status == 0
    }
    /// Last lines of stderr for a fail-closed error message (mirrors
    /// `build_rootfs`'s 12-line tail — enough to diagnose, small enough to log).
    fn stderr_tail(&self) -> String {
        let lines: Vec<&str> = self.stderr.lines().rev().take(12).collect();
        lines.into_iter().rev().collect::<Vec<_>>().join("\n")
    }
}

/// Synchronous command seam for the import build (fake-able in tests).
pub trait ImportCommandRunner: Send + Sync {
    fn run(&self, program: &str, args: &[&str]) -> std::io::Result<ImportCommandOutput>;
}

/// The real runner: `std::process::Command`, capture both streams.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemImportCommandRunner;

impl ImportCommandRunner for SystemImportCommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> std::io::Result<ImportCommandOutput> {
        let output = std::process::Command::new(program).args(args).output()?;
        Ok(ImportCommandOutput {
            status: output.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }
}

/// The probed build tool + its version string (recorded in the receipt).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildToolProbe {
    pub tool: BuildTool,
    pub version: String,
}

fn version_of(runner: &dyn ImportCommandRunner, program: &str) -> Option<String> {
    match runner.run(program, &["--version"]) {
        Ok(out) if out.success() => Some(out.stdout.trim().to_string()),
        _ => None,
    }
}

/// Find the build tool this host will drive: `podman` preferred, `docker`
/// fallback. Fail-closed with a specific message when only `buildah` exists
/// (present but not driven in v0) or when nothing is found.
pub fn probe_build_tool(runner: &dyn ImportCommandRunner) -> Result<BuildToolProbe, String> {
    if let Some(version) = version_of(runner, "podman") {
        return Ok(BuildToolProbe {
            tool: BuildTool::Podman,
            version,
        });
    }
    if let Some(version) = version_of(runner, "docker") {
        return Ok(BuildToolProbe {
            tool: BuildTool::Docker,
            version,
        });
    }
    if version_of(runner, "buildah").is_some() {
        return Err(
            "buildah is present but import v0 drives the docker-compatible CLI only — \
             install podman (preferred, rootless) or docker on this builder host"
                .into(),
        );
    }
    Err("no container build tool found — install podman (preferred, rootless) or docker on this builder host".into())
}

/// One `FROM` in a Dockerfile after ARG substitution: a registry reference
/// (never a prior stage name, never `scratch`).
fn is_stage_ref(candidate: &str, stages: &[String]) -> bool {
    stages.iter().any(|s| s.eq_ignore_ascii_case(candidate))
}

/// Substitute `${VAR}`, `${VAR:-default}`, and `$VAR` in a FROM reference from
/// declared pre-FROM `ARG` defaults + the spec's build args (build args win).
/// Unresolved references fail closed — a base image identity must never be
/// guessed.
fn substitute_from_ref(raw: &str, args: &BTreeMap<String, String>) -> Result<String, String> {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('{') => {
                chars.next();
                let mut body = String::new();
                let mut closed = false;
                for c in chars.by_ref() {
                    if c == '}' {
                        closed = true;
                        break;
                    }
                    body.push(c);
                }
                if !closed {
                    return Err(format!("FROM {raw:?}: unterminated ${{…}} substitution"));
                }
                let (name, default) = match body.split_once(":-") {
                    Some((n, d)) => (n.to_string(), Some(d.to_string())),
                    None => (body.clone(), None),
                };
                match args.get(&name).cloned().or(default) {
                    Some(v) => out.push_str(&v),
                    None => {
                        return Err(format!(
                            "FROM {raw:?}: ARG {name:?} is not declared with a default and no \
                             build arg supplies it (fail-closed: base image identity must be resolvable)"
                        ));
                    }
                }
            }
            _ => {
                let mut name = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_ascii_alphanumeric() || c == '_' {
                        name.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if name.is_empty() {
                    return Err(format!("FROM {raw:?}: dangling '$'"));
                }
                match args.get(&name) {
                    Some(v) => out.push_str(v),
                    None => {
                        return Err(format!(
                            "FROM {raw:?}: ARG {name:?} is not declared with a default and no \
                             build arg supplies it (fail-closed: base image identity must be resolvable)"
                        ));
                    }
                }
            }
        }
    }
    Ok(out)
}

/// Parse the REGISTRY base references out of a (possibly multi-stage)
/// Dockerfile: every `FROM [--platform=…] <ref> [AS <stage>]` whose `<ref>` is
/// not a prior stage name and not `scratch`, with pre-FROM `ARG` defaults +
/// build args substituted. Deduped, source order. Fail-closed on unresolvable
/// substitutions.
///
/// Deliberately NOT a full Dockerfile parser: only `FROM` and pre-FROM `ARG`
/// are interpreted (the build tool parses everything for real); this exists so
/// the importer knows which registry images to pull + digest-pin.
pub fn parse_dockerfile_base_refs(
    dockerfile: &str,
    build_args: &BTreeMap<String, String>,
) -> Result<Vec<String>, String> {
    let mut args: BTreeMap<String, String> = BTreeMap::new();
    let mut seen_from = false;
    let mut stages: Vec<String> = Vec::new();
    let mut refs: Vec<String> = Vec::new();

    for raw_line in dockerfile.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut tokens = line.split_whitespace();
        let Some(instruction) = tokens.next() else {
            continue;
        };
        if instruction.eq_ignore_ascii_case("ARG") && !seen_from {
            // Pre-FROM ARG: `ARG NAME[=default]` — defaults feed FROM substitution;
            // a supplied build arg overrides the default.
            if let Some(decl) = tokens.next() {
                let (name, default) = match decl.split_once('=') {
                    Some((n, d)) => (n.to_string(), Some(d.trim_matches('"').to_string())),
                    None => (decl.to_string(), None),
                };
                if let Some(supplied) = build_args.get(&name) {
                    args.insert(name, supplied.clone());
                } else if let Some(d) = default {
                    args.insert(name, d);
                }
            }
            continue;
        }
        if !instruction.eq_ignore_ascii_case("FROM") {
            continue;
        }
        seen_from = true;
        let mut rest: Vec<&str> = tokens.collect();
        // `--platform=…` flag(s) precede the ref.
        while rest.first().is_some_and(|t| t.starts_with("--")) {
            rest.remove(0);
        }
        let Some(raw_ref) = rest.first() else {
            return Err("FROM with no image reference".into());
        };
        let resolved = substitute_from_ref(raw_ref, &args)?;
        // A FROM may reference any EARLIER stage by name — check against the
        // stages seen so far, BEFORE recording this line's own `AS <stage>`.
        let is_prior_stage = is_stage_ref(&resolved, &stages);
        if rest.len() >= 3 && rest[1].eq_ignore_ascii_case("AS") {
            stages.push(rest[2].to_string());
        }
        if resolved.eq_ignore_ascii_case("scratch") || is_prior_stage {
            continue;
        }
        if !refs.contains(&resolved) {
            refs.push(resolved);
        }
    }
    if !seen_from {
        return Err("Dockerfile has no FROM instruction".into());
    }
    Ok(refs)
}

/// Rewrite the Dockerfile so every REGISTRY `FROM` uses its resolved digest —
/// the digest pin becomes an enforced BUILD INPUT, not an advisory record
/// (review blocker on ato#994 PR 3: building from the original Dockerfile only
/// proved the digest was pulled, not that the build consumed it).
///
/// Rules: registry refs (after the same ARG substitution as
/// [`parse_dockerfile_base_refs`]) are replaced by `digest_by_ref[ref]`
/// (fail-closed when missing); `--platform` flags and `AS <stage>` aliases are
/// preserved; prior-stage `FROM`s and `scratch` pass through verbatim; every
/// non-FROM line is byte-preserved.
pub fn render_effective_dockerfile(
    dockerfile: &str,
    build_args: &BTreeMap<String, String>,
    digest_by_ref: &BTreeMap<String, String>,
) -> Result<String, String> {
    let mut args: BTreeMap<String, String> = BTreeMap::new();
    let mut seen_from = false;
    let mut stages: Vec<String> = Vec::new();
    let mut out: Vec<String> = Vec::new();
    for raw_line in dockerfile.lines() {
        let line = raw_line.trim();
        let mut rewritten: Option<String> = None;
        if !line.is_empty() && !line.starts_with('#') {
            let mut tokens = line.split_whitespace();
            let instruction = tokens.next().unwrap_or_default();
            if instruction.eq_ignore_ascii_case("ARG") && !seen_from {
                // Same pre-FROM ARG collection as the parser (defaults feed FROM
                // substitution; supplied build args win).
                if let Some(decl) = tokens.next() {
                    let (name, default) = match decl.split_once('=') {
                        Some((n, d)) => (n.to_string(), Some(d.trim_matches('"').to_string())),
                        None => (decl.to_string(), None),
                    };
                    if let Some(supplied) = build_args.get(&name) {
                        args.insert(name, supplied.clone());
                    } else if let Some(d) = default {
                        args.insert(name, d);
                    }
                }
            } else if instruction.eq_ignore_ascii_case("FROM") {
                seen_from = true;
                let rest: Vec<&str> = tokens.collect();
                let mut idx = 0;
                while rest.get(idx).is_some_and(|t| t.starts_with("--")) {
                    idx += 1;
                }
                let raw_ref = rest.get(idx).ok_or("FROM with no image reference")?;
                let resolved = substitute_from_ref(raw_ref, &args)?;
                let is_prior_stage = is_stage_ref(&resolved, &stages);
                let as_clause = if rest.len() >= idx + 3 && rest[idx + 1].eq_ignore_ascii_case("AS")
                {
                    stages.push(rest[idx + 2].to_string());
                    Some((rest[idx + 1], rest[idx + 2]))
                } else {
                    None
                };
                if !is_prior_stage && !resolved.eq_ignore_ascii_case("scratch") {
                    let digest_ref = digest_by_ref.get(&resolved).ok_or_else(|| {
                        format!("no resolved digest for base ref {resolved:?} — refusing to render an unpinned effective Dockerfile (fail-closed)")
                    })?;
                    let mut parts: Vec<String> = vec![instruction.to_string()];
                    parts.extend(rest[..idx].iter().map(|s| s.to_string()));
                    parts.push(digest_ref.clone());
                    if let Some((as_kw, name)) = as_clause {
                        parts.push(as_kw.to_string());
                        parts.push(name.to_string());
                    }
                    rewritten = Some(parts.join(" "));
                }
            }
        }
        out.push(rewritten.unwrap_or_else(|| raw_line.to_string()));
    }
    Ok(out.join("\n") + "\n")
}

/// The final image's runtime config, extracted from `image inspect` — the
/// input the next slice maps into `ServiceSpec` (CMD/ENTRYPOINT/WORKDIR/ENV/
/// EXPOSE) and into the `docker_user_ignored` / `docker_healthcheck_ignored`
/// warnings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DockerImageConfig {
    pub entrypoint: Vec<String>,
    pub cmd: Vec<String>,
    pub working_dir: Option<String>,
    /// Post-build resolved env (`Config.Env`, split on the first `=`). Includes
    /// base-image inheritance (PATH etc.) — the secret gate runs on ALL of it.
    pub env: BTreeMap<String, String>,
    /// TCP ports from `Config.ExposedPorts` keys (`"8080/tcp"`), sorted. UDP
    /// entries are skipped (a snapshot public service is an HTTP/TCP target).
    pub exposed_tcp_ports: Vec<u16>,
    /// Raw `Config.User` when non-empty (mapping is NOT honored in v0 —
    /// the next slice emits `docker_user_ignored`).
    pub user: Option<String>,
    /// `Config.Healthcheck` present (not honored in v0 —
    /// `docker_healthcheck_ignored`).
    pub has_healthcheck: bool,
    /// `Config.Volumes` keys (`VOLUME` directives). v0 REJECTS these at plan
    /// derivation unless the author maps them to Ato `[state]` — an unmapped
    /// volume would silently lose data on a frozen-snapshot resume (the exact
    /// ato#983 lesson durable state exists to prevent).
    pub volumes: Vec<String>,
}

/// Everything the build step proved about the image, for the receipt + the
/// next slice. No rootfs bytes yet — export/injection is the next PR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerfileBuildOutput {
    pub image_tag: String,
    /// The built image's content identity (`.Id`, `sha256:…`). Local builds
    /// have no RepoDigest — the ID is the reproducible reference we record.
    pub final_image_digest: String,
    pub resolved_base_images: Vec<ResolvedBaseImage>,
    pub image_config: DockerImageConfig,
    /// sha256 of the ORIGINAL Dockerfile (as authored).
    pub dockerfile_sha256: String,
    /// sha256 of the EFFECTIVE Dockerfile the build consumed (registry FROMs
    /// digest-pinned via [`render_effective_dockerfile`]).
    pub effective_dockerfile_sha256: String,
    pub build_context_digest: String,
}

fn run_tool(
    runner: &dyn ImportCommandRunner,
    tool: BuildTool,
    args: &[&str],
    what: &str,
) -> Result<ImportCommandOutput, String> {
    let out = runner
        .run(tool.as_str(), args)
        .map_err(|e| format!("{what}: spawn {} failed: {e}", tool.as_str()))?;
    if !out.success() {
        return Err(format!(
            "{what} failed (exit {}): {}",
            out.status,
            out.stderr_tail()
        ));
    }
    Ok(out)
}

/// Pull every base ref for [`DOCKER_IMPORT_PLATFORM`] and resolve it to a
/// digest-pinned identity via `image inspect --format {{index .RepoDigests 0}}`.
/// A ref that cannot be digest-resolved fails the import — a tag is not a
/// reproducible identity (ato#994 base-image policy).
fn resolve_base_digests(
    runner: &dyn ImportCommandRunner,
    tool: BuildTool,
    refs: &[String],
) -> Result<Vec<ResolvedBaseImage>, String> {
    let mut resolved = Vec::with_capacity(refs.len());
    for r in refs {
        run_tool(
            runner,
            tool,
            &["pull", "--platform", DOCKER_IMPORT_PLATFORM, r],
            &format!("pull base image {r:?}"),
        )?;
        let out = run_tool(
            runner,
            tool,
            &[
                "image",
                "inspect",
                "--format",
                "{{index .RepoDigests 0}}",
                r,
            ],
            &format!("inspect base image {r:?}"),
        )?;
        let digest_ref = out.stdout.trim().to_string();
        if digest_ref.is_empty() || !digest_ref.contains("@sha256:") {
            return Err(format!(
                "base image {r:?} did not resolve to a registry digest (got {digest_ref:?}) — \
                 a tag is not a reproducible identity; refusing to continue (fail-closed)"
            ));
        }
        resolved.push(ResolvedBaseImage {
            original_ref: r.clone(),
            resolved_digest: digest_ref,
        });
    }
    Ok(resolved)
}

fn parse_image_config(inspect_json: &str) -> Result<DockerImageConfig, String> {
    let v: serde_json::Value = serde_json::from_str(inspect_json)
        .map_err(|e| format!("image inspect JSON parse error: {e}"))?;
    // docker/podman `image inspect` emit an array; podman can also emit a bare object.
    let obj = v.get(0).unwrap_or(&v);
    let config = obj
        .get("Config")
        .ok_or("image inspect output has no Config object")?;
    let str_vec = |key: &str| -> Vec<String> {
        config
            .get(key)
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut env = BTreeMap::new();
    for kv in str_vec("Env") {
        if let Some((k, val)) = kv.split_once('=') {
            env.insert(k.to_string(), val.to_string());
        }
    }
    let mut exposed_tcp_ports: Vec<u16> = config
        .get("ExposedPorts")
        .and_then(|x| x.as_object())
        .map(|m| {
            m.keys()
                .filter_map(|k| k.strip_suffix("/tcp").and_then(|p| p.parse::<u16>().ok()))
                .collect()
        })
        .unwrap_or_default();
    exposed_tcp_ports.sort_unstable();
    exposed_tcp_ports.dedup();
    Ok(DockerImageConfig {
        entrypoint: str_vec("Entrypoint"),
        cmd: str_vec("Cmd"),
        working_dir: config
            .get("WorkingDir")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from),
        env,
        exposed_tcp_ports,
        user: config
            .get("User")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from),
        has_healthcheck: config.get("Healthcheck").is_some_and(|h| !h.is_null()),
        volumes: {
            let mut v: Vec<String> = config
                .get("Volumes")
                .and_then(|x| x.as_object())
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default();
            v.sort();
            v
        },
    })
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

/// Streaming sha256 of a file (the packed ext4 can be GiB-scale — never read
/// it whole into memory).
pub(crate) fn sha256_file_hex(path: &Path) -> Result<String, String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}

/// Deterministic digest over the build context actually sent to the build:
/// sorted relative paths, each contributing `path\0content-sha256\n` (symlinks
/// contribute their target string; directories only via their children).
/// `.git` is excluded (not app content; its object layout is not part of what
/// the build consumes in any Dockerfile we import).
///
/// v0 divergence, on purpose: `.dockerignore` is NOT honored — the digest may
/// be stricter (more inputs) than what the build tool sends, never looser.
/// A stricter identity can only cause a spurious rebuild, never a stale reuse.
pub fn build_context_digest(context_dir: &Path) -> Result<String, String> {
    let mut entries: Vec<(String, String)> = Vec::new();
    let mut stack = vec![context_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let rd = std::fs::read_dir(&dir).map_err(|e| format!("read_dir {}: {e}", dir.display()))?;
        for entry in rd {
            let entry = entry.map_err(|e| format!("read_dir entry: {e}"))?;
            let path = entry.path();
            let rel = path
                .strip_prefix(context_dir)
                .map_err(|_| "context walk escaped the context dir".to_string())?
                .to_string_lossy()
                .to_string();
            if rel == ".git" || rel.starts_with(".git/") {
                continue;
            }
            let ft = entry
                .file_type()
                .map_err(|e| format!("file_type {rel}: {e}"))?;
            if ft.is_dir() {
                stack.push(path);
            } else if ft.is_symlink() {
                let target =
                    std::fs::read_link(&path).map_err(|e| format!("read_link {rel}: {e}"))?;
                entries.push((rel, sha256_hex(target.to_string_lossy().as_bytes())));
            } else {
                let bytes = std::fs::read(&path).map_err(|e| format!("read {rel}: {e}"))?;
                entries.push((rel, sha256_hex(&bytes)));
            }
        }
    }
    entries.sort();
    let mut h = Sha256::new();
    for (rel, digest) in &entries {
        h.update(rel.as_bytes());
        h.update([0u8]);
        h.update(digest.as_bytes());
        h.update([b'\n']);
    }
    Ok(format!("{:x}", h.finalize()))
}

/// Execute the Dockerfile build: read + hash the Dockerfile, digest the
/// context, pull + digest-pin every base image, `build --platform linux/amd64`
/// (multi-stage handled by the tool; the FINAL stage is what `-t` tags), then
/// inspect the result for its identity + runtime config. No export, no
/// injection — that is the next slice.
pub fn run_dockerfile_build(
    runner: &dyn ImportCommandRunner,
    probe: &BuildToolProbe,
    context_dir: &Path,
    spec: &DockerImportSpec,
    image_tag: &str,
) -> Result<DockerfileBuildOutput, String> {
    // Containment: the spec's lexical validation already rejected `..`/absolute;
    // canonicalize to close symlink traversal, same discipline as
    // `contained_source_root`.
    let dockerfile_path = context_dir.join(&spec.dockerfile_path);
    let context_canon = context_dir
        .canonicalize()
        .map_err(|e| format!("canonicalize context dir: {e}"))?;
    let dockerfile_canon = dockerfile_path
        .canonicalize()
        .map_err(|e| format!("Dockerfile {} not found: {e}", dockerfile_path.display()))?;
    if !dockerfile_canon.starts_with(&context_canon) {
        return Err(format!(
            "Dockerfile {} escapes the build context {}",
            dockerfile_canon.display(),
            context_canon.display()
        ));
    }

    let dockerfile_bytes =
        std::fs::read(&dockerfile_canon).map_err(|e| format!("read Dockerfile: {e}"))?;
    let dockerfile_sha256 = sha256_hex(&dockerfile_bytes);
    let dockerfile_text = String::from_utf8_lossy(&dockerfile_bytes);

    let base_refs = parse_dockerfile_base_refs(&dockerfile_text, &spec.build_args)?;
    let resolved_base_images = resolve_base_digests(runner, probe.tool, &base_refs)?;

    let context_digest = build_context_digest(&context_canon)?;

    // Render + stage the EFFECTIVE Dockerfile (registry FROMs → resolved
    // digests) OUTSIDE the context dir, so the context digest is unaffected
    // and the author's Dockerfile is never touched. The build consumes this
    // file — the digest pin is thereby enforced, not advisory.
    let digest_by_ref: BTreeMap<String, String> = resolved_base_images
        .iter()
        .map(|r| (r.original_ref.clone(), r.resolved_digest.clone()))
        .collect();
    let effective =
        render_effective_dockerfile(&dockerfile_text, &spec.build_args, &digest_by_ref)?;
    let effective_dockerfile_sha256 = sha256_hex(effective.as_bytes());
    // Unique per invocation (pid + sequence): concurrent imports — including
    // parallel tests sharing a tag — must never share/steal a staging dir.
    static EFF_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let eff_dir = std::env::temp_dir().join(format!(
        "ato-docker-import-{}-{}-{}",
        image_tag.replace(['/', ':'], "-"),
        std::process::id(),
        EFF_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&eff_dir)
        .map_err(|e| format!("create effective-Dockerfile dir: {e}"))?;
    let eff_path = eff_dir.join("Dockerfile.effective");
    std::fs::write(&eff_path, &effective)
        .map_err(|e| format!("write effective Dockerfile: {e}"))?;

    let dockerfile_arg = eff_path.to_string_lossy().to_string();
    let context_arg = context_canon.to_string_lossy().to_string();
    let mut build_args_flat: Vec<String> = Vec::new();
    for (k, v) in &spec.build_args {
        build_args_flat.push("--build-arg".into());
        build_args_flat.push(format!("{k}={v}"));
    }
    let mut args: Vec<&str> = vec![
        "build",
        "--platform",
        DOCKER_IMPORT_PLATFORM,
        "-f",
        &dockerfile_arg,
        "-t",
        image_tag,
    ];
    args.extend(build_args_flat.iter().map(|s| s.as_str()));
    args.push(&context_arg);
    let build_result = run_tool(runner, probe.tool, &args, "dockerfile build");
    let _ = std::fs::remove_dir_all(&eff_dir);
    build_result?;

    let id_out = run_tool(
        runner,
        probe.tool,
        &["image", "inspect", "--format", "{{.Id}}", image_tag],
        "inspect built image id",
    )?;
    let mut final_image_digest = id_out.stdout.trim().to_string();
    if final_image_digest.is_empty() {
        return Err("built image has no Id".into());
    }
    // podman prints bare hex for .Id; docker prints sha256:… — normalize.
    if !final_image_digest.starts_with("sha256:") {
        final_image_digest = format!("sha256:{final_image_digest}");
    }

    let cfg_out = run_tool(
        runner,
        probe.tool,
        &["image", "inspect", image_tag],
        "inspect built image config",
    )?;
    let image_config = parse_image_config(&cfg_out.stdout)?;

    Ok(DockerfileBuildOutput {
        image_tag: image_tag.to_string(),
        final_image_digest,
        resolved_base_images,
        image_config,
        dockerfile_sha256,
        effective_dockerfile_sha256,
        build_context_digest: context_digest,
    })
}

// ── ato#1028 Registry Image Import v1.8: the OCI-image ACQUIRE stage ──────────
//
// The ONLY step that differs from the Dockerfile lane (pull+inspect vs build).
// Everything downstream — plan derivation, rootfs pack, receipt, seal — is
// shared with `run_dockerfile_import` unchanged.

/// The result of pulling + inspecting a public registry image for the OCI import
/// lane ([`super::run_oci_image_import`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PulledImage {
    /// The registry MANIFEST digest the pull resolved to
    /// (`registry/repo@sha256:…`) for [`DOCKER_IMPORT_PLATFORM`] — the pinned
    /// identity the artifact keys on. A tag is not a reproducible identity
    /// (same rule as [`resolve_base_digests`]); two tags of the same image
    /// resolve here identically, so they yield the same artifact identity.
    pub resolved_digest: String,
    /// The local image content id (`.Id`, `sha256:…`) — the exact image bytes
    /// that run (the same field the Dockerfile lane records as its final image).
    pub final_image_digest: String,
    /// The runtime config — the SAME [`DockerImageConfig`] type the Dockerfile
    /// lane feeds to `derive_imported_service_plan_with_volumes`.
    pub image_config: DockerImageConfig,
}

/// Pull a PUBLIC registry image for [`DOCKER_IMPORT_PLATFORM`] and inspect it:
/// resolve its registry manifest digest (a tag is not a reproducible identity —
/// ato#994 base-image policy), its content id, and its runtime config. No auth
/// (v1 public-only). This is the OCI-image import lane's acquire step; the image
/// is pulled by the caller-supplied ref (tag OR digest) and pinned to the
/// resolved digest before anything downstream consumes it.
pub fn pull_and_inspect_image(
    runner: &dyn ImportCommandRunner,
    tool: BuildTool,
    image_ref: &str,
) -> Result<PulledImage, String> {
    run_tool(
        runner,
        tool,
        &["pull", "--platform", DOCKER_IMPORT_PLATFORM, image_ref],
        &format!("pull image {image_ref:?}"),
    )?;
    let dig = run_tool(
        runner,
        tool,
        &["image", "inspect", "--format", "{{index .RepoDigests 0}}", image_ref],
        &format!("resolve image digest {image_ref:?}"),
    )?;
    let resolved_digest = dig.stdout.trim().to_string();
    if resolved_digest.is_empty() || !resolved_digest.contains("@sha256:") {
        return Err(format!(
            "image {image_ref:?} did not resolve to a registry digest (got {resolved_digest:?}) — \
             a tag is not a reproducible identity; refusing to continue (fail-closed)"
        ));
    }
    let id_out = run_tool(
        runner,
        tool,
        &["image", "inspect", "--format", "{{.Id}}", image_ref],
        &format!("inspect image id {image_ref:?}"),
    )?;
    let mut final_image_digest = id_out.stdout.trim().to_string();
    if final_image_digest.is_empty() {
        return Err(format!("image {image_ref:?} has no Id"));
    }
    // podman prints bare hex for .Id; docker prints sha256:… — normalize.
    if !final_image_digest.starts_with("sha256:") {
        final_image_digest = format!("sha256:{final_image_digest}");
    }
    let cfg_out = run_tool(
        runner,
        tool,
        &["image", "inspect", image_ref],
        &format!("inspect image config {image_ref:?}"),
    )?;
    let image_config = parse_image_config(&cfg_out.stdout)?;
    Ok(PulledImage { resolved_digest, final_image_digest, image_config })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Scripted fake: matches invocations by (program, first tokens) and
    /// records every call for order/shape assertions.
    struct FakeRunner {
        script: Vec<(String, ImportCommandOutput)>,
        calls: Mutex<Vec<String>>,
    }

    impl FakeRunner {
        fn new(script: Vec<(&str, i32, &str, &str)>) -> Self {
            FakeRunner {
                script: script
                    .into_iter()
                    .map(|(prefix, status, stdout, stderr)| {
                        (
                            prefix.to_string(),
                            ImportCommandOutput {
                                status,
                                stdout: stdout.to_string(),
                                stderr: stderr.to_string(),
                            },
                        )
                    })
                    .collect(),
                calls: Mutex::new(Vec::new()),
            }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl ImportCommandRunner for FakeRunner {
        fn run(&self, program: &str, args: &[&str]) -> std::io::Result<ImportCommandOutput> {
            let full = format!("{program} {}", args.join(" "));
            self.calls.lock().unwrap().push(full.clone());
            for (prefix, out) in &self.script {
                if full.starts_with(prefix.as_str()) {
                    return Ok(out.clone());
                }
            }
            // Unscripted command = "not installed" (io error), like a missing binary.
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("unscripted: {full}"),
            ))
        }
    }

    // --- probe_build_tool -------------------------------------------------------

    #[test]
    fn probe_prefers_podman_then_docker() {
        let r = FakeRunner::new(vec![
            ("podman --version", 0, "podman version 4.9.3\n", ""),
            ("docker --version", 0, "Docker version 26.0.0\n", ""),
        ]);
        let p = probe_build_tool(&r).unwrap();
        assert_eq!(p.tool, BuildTool::Podman);
        assert_eq!(p.version, "podman version 4.9.3");

        let r = FakeRunner::new(vec![("docker --version", 0, "Docker version 26.0.0\n", "")]);
        let p = probe_build_tool(&r).unwrap();
        assert_eq!(p.tool, BuildTool::Docker);
    }

    #[test]
    fn probe_buildah_only_is_a_specific_error() {
        let r = FakeRunner::new(vec![(
            "buildah --version",
            0,
            "buildah version 1.35.0\n",
            "",
        )]);
        let err = probe_build_tool(&r).unwrap_err();
        assert!(err.contains("buildah is present"), "{err}");
        assert!(err.contains("podman"), "{err}");
    }

    #[test]
    fn probe_nothing_found_fails_closed() {
        let r = FakeRunner::new(vec![]);
        let err = probe_build_tool(&r).unwrap_err();
        assert!(err.contains("no container build tool"), "{err}");
    }

    // --- parse_dockerfile_base_refs ----------------------------------------------

    fn no_args() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    #[test]
    fn single_stage_from_is_collected() {
        let refs = parse_dockerfile_base_refs("FROM node:20-slim\nRUN true\n", &no_args()).unwrap();
        assert_eq!(refs, vec!["node:20-slim"]);
    }

    #[test]
    fn multi_stage_collects_registry_refs_and_skips_stage_refs() {
        let df = r#"
# build stage
FROM golang:1.22 AS builder
RUN make

FROM --platform=linux/amd64 alpine:3.19 AS runtime
COPY --from=builder /out /app

FROM runtime
CMD ["/app/serve"]
"#;
        let refs = parse_dockerfile_base_refs(df, &no_args()).unwrap();
        assert_eq!(refs, vec!["golang:1.22", "alpine:3.19"]);
    }

    #[test]
    fn stage_names_match_case_insensitively_and_scratch_is_skipped() {
        let df = "FROM golang:1.22 AS Builder\nFROM scratch\nCOPY --from=BUILDER /bin /bin\n";
        let refs = parse_dockerfile_base_refs(df, &no_args()).unwrap();
        assert_eq!(refs, vec!["golang:1.22"]);
    }

    #[test]
    fn arg_defaults_and_build_args_substitute_from_refs() {
        let df = "ARG BASE=node:20\nFROM ${BASE}\n";
        assert_eq!(
            parse_dockerfile_base_refs(df, &no_args()).unwrap(),
            vec!["node:20"]
        );

        let mut args = no_args();
        args.insert("BASE".into(), "node:22".into());
        assert_eq!(
            parse_dockerfile_base_refs(df, &args).unwrap(),
            vec!["node:22"]
        );

        // ${VAR:-default} form.
        let df = "FROM ${BASE:-python:3.12-slim}\n";
        assert_eq!(
            parse_dockerfile_base_refs(df, &no_args()).unwrap(),
            vec!["python:3.12-slim"]
        );

        // $VAR form with a declared default.
        let df = "ARG REG=docker.io\nFROM $REG/library/redis:7\n";
        assert_eq!(
            parse_dockerfile_base_refs(df, &no_args()).unwrap(),
            vec!["docker.io/library/redis:7"]
        );
    }

    #[test]
    fn unresolved_arg_fails_closed() {
        let err = parse_dockerfile_base_refs("FROM ${MYSTERY}\n", &no_args()).unwrap_err();
        assert!(
            err.contains("MYSTERY") && err.contains("fail-closed"),
            "{err}"
        );
    }

    #[test]
    fn no_from_at_all_is_an_error() {
        let err = parse_dockerfile_base_refs("# empty\nRUN true\n", &no_args()).unwrap_err();
        assert!(err.contains("no FROM"), "{err}");
    }

    #[test]
    fn duplicate_refs_dedupe() {
        let df = "FROM alpine:3.19 AS a\nFROM alpine:3.19 AS b\n";
        assert_eq!(
            parse_dockerfile_base_refs(df, &no_args()).unwrap(),
            vec!["alpine:3.19"]
        );
    }

    // --- render_effective_dockerfile ------------------------------------------------

    fn digests(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn effective_rewrites_registry_from_and_keeps_stage_alias() {
        let out = render_effective_dockerfile(
            "FROM node:20 AS builder\nRUN make\n",
            &no_args(),
            &digests(&[("node:20", "docker.io/library/node@sha256:aaaa")]),
        )
        .unwrap();
        assert!(
            out.contains("FROM docker.io/library/node@sha256:aaaa AS builder"),
            "{out}"
        );
        assert!(!out.contains("FROM node:20 "), "{out}");
        assert!(out.contains("RUN make"), "{out}");
    }

    #[test]
    fn effective_leaves_prior_stage_from_untouched() {
        let df = "FROM golang:1.22 AS builder\nFROM builder AS runtime\nCMD [\"/bin/x\"]\n";
        let out = render_effective_dockerfile(
            df,
            &no_args(),
            &digests(&[("golang:1.22", "docker.io/library/golang@sha256:bbbb")]),
        )
        .unwrap();
        assert!(
            out.contains("FROM docker.io/library/golang@sha256:bbbb AS builder"),
            "{out}"
        );
        assert!(out.contains("FROM builder AS runtime"), "{out}"); // verbatim
    }

    #[test]
    fn effective_pins_every_registry_ref_in_multi_stage_and_keeps_flags() {
        let df = "ARG BASE=alpine:3.19\nFROM --platform=linux/amd64 golang:1.22 AS b\nFROM ${BASE}\nCOPY --from=b /out /app\n";
        let out = render_effective_dockerfile(
            df,
            &no_args(),
            &digests(&[
                ("golang:1.22", "docker.io/library/golang@sha256:bbbb"),
                ("alpine:3.19", "docker.io/library/alpine@sha256:cccc"),
            ]),
        )
        .unwrap();
        assert!(
            out.contains("FROM --platform=linux/amd64 docker.io/library/golang@sha256:bbbb AS b"),
            "{out}"
        );
        assert!(
            out.contains("FROM docker.io/library/alpine@sha256:cccc"),
            "{out}"
        );
        // scratch/comments/other lines byte-preserved; ARG line kept.
        assert!(out.contains("ARG BASE=alpine:3.19"), "{out}");
        assert!(out.contains("COPY --from=b /out /app"), "{out}");
    }

    #[test]
    fn effective_fails_closed_on_a_missing_digest() {
        let err =
            render_effective_dockerfile("FROM node:20\n", &no_args(), &digests(&[])).unwrap_err();
        assert!(
            err.contains("no resolved digest") && err.contains("fail-closed"),
            "{err}"
        );
    }

    // --- parse_image_config --------------------------------------------------------

    const INSPECT_JSON: &str = r#"[
      {
        "Id": "sha256:0123456789abcdef",
        "Config": {
          "User": "app",
          "ExposedPorts": {"8080/tcp": {}, "9229/udp": {}, "3000/tcp": {}},
          "Env": ["PATH=/usr/local/bin:/usr/bin", "PORT=8080", "NODE_ENV=production"],
          "Entrypoint": ["docker-entrypoint.sh"],
          "Cmd": ["node", "server.js"],
          "WorkingDir": "/app",
          "Healthcheck": {"Test": ["CMD", "curl", "-f", "http://localhost:8080/"]}
        }
      }
    ]"#;

    #[test]
    fn image_config_extracts_the_mapping_inputs() {
        let cfg = parse_image_config(INSPECT_JSON).unwrap();
        assert_eq!(cfg.entrypoint, vec!["docker-entrypoint.sh"]);
        assert_eq!(cfg.cmd, vec!["node", "server.js"]);
        assert_eq!(cfg.working_dir.as_deref(), Some("/app"));
        assert_eq!(cfg.env["PORT"], "8080");
        assert_eq!(cfg.exposed_tcp_ports, vec![3000, 8080]); // sorted, udp skipped
        assert_eq!(cfg.user.as_deref(), Some("app"));
        assert!(cfg.has_healthcheck);
    }

    #[test]
    fn image_config_handles_bare_object_and_missing_fields() {
        let cfg = parse_image_config(r#"{"Config": {"Cmd": ["/bin/sh"]}}"#).unwrap();
        assert_eq!(cfg.cmd, vec!["/bin/sh"]);
        assert!(cfg.entrypoint.is_empty());
        assert!(cfg.env.is_empty());
        assert!(cfg.exposed_tcp_ports.is_empty());
        assert!(cfg.user.is_none());
        assert!(!cfg.has_healthcheck);
        assert!(parse_image_config(r#"[{}]"#).is_err());
    }

    // --- build_context_digest --------------------------------------------------------

    #[test]
    fn context_digest_is_deterministic_and_content_sensitive() {
        let dir = tempdir();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("Dockerfile"), "FROM alpine:3.19\n").unwrap();
        std::fs::write(dir.join("src/main.py"), "print('hi')\n").unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();

        let a = build_context_digest(&dir).unwrap();
        let b = build_context_digest(&dir).unwrap();
        assert_eq!(a, b);

        // .git content must not affect the digest.
        std::fs::write(dir.join(".git/HEAD"), "ref: refs/heads/other\n").unwrap();
        assert_eq!(build_context_digest(&dir).unwrap(), a);

        // App content must.
        std::fs::write(dir.join("src/main.py"), "print('changed')\n").unwrap();
        assert_ne!(build_context_digest(&dir).unwrap(), a);
        cleanup(dir);
    }

    // --- run_dockerfile_build ----------------------------------------------------------

    fn tempdir() -> std::path::PathBuf {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "docker-import-test-{}-{}-{}",
            std::process::id(),
            std::thread::current()
                .name()
                .unwrap_or("t")
                .replace("::", "-"),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
    fn cleanup(dir: std::path::PathBuf) {
        let _ = std::fs::remove_dir_all(dir);
    }

    fn full_build_script() -> Vec<(&'static str, i32, &'static str, &'static str)> {
        vec![
            ("podman pull", 0, "", ""),
            (
                "podman image inspect --format {{index .RepoDigests 0}}",
                0,
                "docker.io/library/node@sha256:aaaa\n",
                "",
            ),
            ("podman build", 0, "", ""),
            ("podman image inspect --format {{.Id}}", 0, "0123abcd\n", ""),
            ("podman image inspect", 0, INSPECT_JSON, ""),
        ]
    }

    #[test]
    fn build_flow_runs_pull_pin_build_inspect_in_order() {
        let dir = tempdir();
        std::fs::write(
            dir.join("Dockerfile"),
            "FROM node:20-slim\nCMD [\"node\"]\n",
        )
        .unwrap();
        let runner = FakeRunner::new(full_build_script());
        let probe = BuildToolProbe {
            tool: BuildTool::Podman,
            version: "podman 4.9".into(),
        };
        let spec = DockerImportSpec::new("Dockerfile", BTreeMap::new()).unwrap();

        let out = run_dockerfile_build(&runner, &probe, &dir, &spec, "ato-import-test").unwrap();
        assert_eq!(out.final_image_digest, "sha256:0123abcd"); // bare hex normalized
        assert_eq!(out.resolved_base_images.len(), 1);
        assert_eq!(out.resolved_base_images[0].original_ref, "node:20-slim");
        assert!(
            out.resolved_base_images[0]
                .resolved_digest
                .contains("@sha256:aaaa")
        );
        assert_eq!(out.image_config.cmd, vec!["node", "server.js"]);
        assert_eq!(out.dockerfile_sha256.len(), 64);
        assert_eq!(out.build_context_digest.len(), 64);

        let calls = runner.calls();
        let idx = |prefix: &str| {
            calls
                .iter()
                .position(|c| c.starts_with(prefix))
                .unwrap_or_else(|| panic!("missing call {prefix}: {calls:?}"))
        };
        assert!(idx("podman pull --platform linux/amd64 node:20-slim") < idx("podman build"));
        assert!(
            idx("podman build --platform linux/amd64 -f")
                < idx("podman image inspect --format {{.Id}}")
        );
        // The build consumes the EFFECTIVE Dockerfile, never the original.
        let build_call = calls
            .iter()
            .find(|c| c.starts_with("podman build"))
            .unwrap();
        assert!(build_call.contains("Dockerfile.effective"), "{build_call}");
        assert!(
            !build_call.contains(&dir.join("Dockerfile").to_string_lossy().to_string()),
            "{build_call}"
        );
        assert_eq!(out.effective_dockerfile_sha256.len(), 64);
        assert_ne!(out.effective_dockerfile_sha256, out.dockerfile_sha256); // digest-pinned rewrite differs
        cleanup(dir);
    }

    /// A runner that captures the CONTENT of the `-f` file at build time —
    /// proving the build input is the digest-pinned effective Dockerfile
    /// (the file is cleaned up after the build, so read-at-call is the only
    /// honest observation point).
    struct EffectiveCapturingRunner {
        inner: FakeRunner,
        captured: Mutex<Option<String>>,
    }
    impl ImportCommandRunner for EffectiveCapturingRunner {
        fn run(&self, program: &str, args: &[&str]) -> std::io::Result<ImportCommandOutput> {
            if args.first() == Some(&"build") {
                let f_idx = args.iter().position(|a| *a == "-f").unwrap();
                let content = std::fs::read_to_string(args[f_idx + 1]).unwrap();
                *self.captured.lock().unwrap() = Some(content);
            }
            self.inner.run(program, args)
        }
    }

    #[test]
    fn build_input_is_the_digest_pinned_effective_dockerfile() {
        let dir = tempdir();
        std::fs::write(
            dir.join("Dockerfile"),
            "FROM node:20-slim\nCMD [\"node\"]\n",
        )
        .unwrap();
        let runner = EffectiveCapturingRunner {
            inner: FakeRunner::new(full_build_script()),
            captured: Mutex::new(None),
        };
        let probe = BuildToolProbe {
            tool: BuildTool::Podman,
            version: "p".into(),
        };
        let spec = DockerImportSpec::new("Dockerfile", BTreeMap::new()).unwrap();
        run_dockerfile_build(&runner, &probe, &dir, &spec, "t").unwrap();

        let effective = runner
            .captured
            .lock()
            .unwrap()
            .clone()
            .expect("build ran with -f");
        assert!(
            effective.contains("FROM docker.io/library/node@sha256:aaaa"),
            "{effective}"
        );
        assert!(!effective.contains("node:20-slim"), "{effective}");
        // The author's Dockerfile on disk is untouched.
        let original = std::fs::read_to_string(dir.join("Dockerfile")).unwrap();
        assert_eq!(original, "FROM node:20-slim\nCMD [\"node\"]\n");
        cleanup(dir);
    }

    #[test]
    fn build_args_are_passed_and_context_is_last() {
        let dir = tempdir();
        std::fs::write(
            dir.join("Dockerfile"),
            "ARG NODE_VERSION=20\nFROM node:${NODE_VERSION}-slim\n",
        )
        .unwrap();
        let mut script = full_build_script();
        script[1].2 = "docker.io/library/node@sha256:bbbb\n";
        let runner = FakeRunner::new(script);
        let probe = BuildToolProbe {
            tool: BuildTool::Podman,
            version: "p".into(),
        };
        let mut args = BTreeMap::new();
        args.insert("NODE_VERSION".to_string(), "22".to_string());
        let spec = DockerImportSpec::new("Dockerfile", args).unwrap();

        let out = run_dockerfile_build(&runner, &probe, &dir, &spec, "t").unwrap();
        // The supplied build arg overrode the ARG default in FROM resolution…
        assert_eq!(out.resolved_base_images[0].original_ref, "node:22-slim");
        // …and was passed to the build, with the context dir as the final arg.
        let build_call = runner
            .calls()
            .into_iter()
            .find(|c| c.starts_with("podman build"))
            .unwrap();
        assert!(
            build_call.contains("--build-arg NODE_VERSION=22"),
            "{build_call}"
        );
        assert!(
            build_call
                .trim_end()
                .ends_with(&dir.canonicalize().unwrap().to_string_lossy().to_string()),
            "{build_call}"
        );
        cleanup(dir);
    }

    #[test]
    fn undigestable_base_ref_fails_closed() {
        let dir = tempdir();
        std::fs::write(dir.join("Dockerfile"), "FROM node:20-slim\n").unwrap();
        let mut script = full_build_script();
        script[1] = (
            "podman image inspect --format {{index .RepoDigests 0}}",
            0,
            "\n",
            "",
        );
        let runner = FakeRunner::new(script);
        let probe = BuildToolProbe {
            tool: BuildTool::Podman,
            version: "p".into(),
        };
        let spec = DockerImportSpec::new("Dockerfile", BTreeMap::new()).unwrap();

        let err = run_dockerfile_build(&runner, &probe, &dir, &spec, "t").unwrap_err();
        assert!(
            err.contains("did not resolve to a registry digest"),
            "{err}"
        );
        // The build must never have run.
        assert!(
            !runner.calls().iter().any(|c| c.starts_with("podman build")),
            "{:?}",
            runner.calls()
        );
        cleanup(dir);
    }

    #[test]
    fn failed_build_surfaces_stderr_tail() {
        let dir = tempdir();
        std::fs::write(dir.join("Dockerfile"), "FROM node:20-slim\n").unwrap();
        let mut script = full_build_script();
        script[2] = (
            "podman build",
            1,
            "",
            "step 3/9: npm ci\nnpm ERR! missing lockfile\n",
        );
        let runner = FakeRunner::new(script);
        let probe = BuildToolProbe {
            tool: BuildTool::Podman,
            version: "p".into(),
        };
        let spec = DockerImportSpec::new("Dockerfile", BTreeMap::new()).unwrap();

        let err = run_dockerfile_build(&runner, &probe, &dir, &spec, "t").unwrap_err();
        assert!(err.contains("dockerfile build failed"), "{err}");
        assert!(err.contains("missing lockfile"), "{err}");
        cleanup(dir);
    }

    #[test]
    fn missing_dockerfile_and_escape_fail_closed() {
        let dir = tempdir();
        let runner = FakeRunner::new(vec![]);
        let probe = BuildToolProbe {
            tool: BuildTool::Podman,
            version: "p".into(),
        };
        let spec = DockerImportSpec::new("Dockerfile", BTreeMap::new()).unwrap();
        let err = run_dockerfile_build(&runner, &probe, &dir, &spec, "t").unwrap_err();
        assert!(err.contains("not found"), "{err}");

        // Symlink escape: Dockerfile -> outside file is caught by canonical containment.
        #[cfg(unix)]
        {
            let outside = tempdir();
            std::fs::write(outside.join("Dockerfile"), "FROM alpine\n").unwrap();
            std::os::unix::fs::symlink(outside.join("Dockerfile"), dir.join("Dockerfile")).unwrap();
            let err = run_dockerfile_build(&runner, &probe, &dir, &spec, "t").unwrap_err();
            assert!(err.contains("escapes the build context"), "{err}");
            cleanup(outside);
        }
        cleanup(dir);
    }

    // --- pull_and_inspect_image (ato#1028 OCI acquire) -----------------------------

    fn full_pull_script(repo_digest: &str) -> Vec<(&'static str, i32, String, &'static str)> {
        vec![
            ("podman pull", 0, String::new(), ""),
            ("podman image inspect --format {{index .RepoDigests 0}}", 0, format!("{repo_digest}\n"), ""),
            ("podman image inspect --format {{.Id}}", 0, "0123abcd\n".to_string(), ""),
            ("podman image inspect", 0, INSPECT_JSON.to_string(), ""),
        ]
    }

    /// Owned-string variant of `FakeRunner::new` (the pull digest is built at runtime).
    fn owned_runner(script: Vec<(&'static str, i32, String, &'static str)>) -> FakeRunner {
        FakeRunner {
            script: script
                .into_iter()
                .map(|(prefix, status, stdout, stderr)| {
                    (prefix.to_string(), ImportCommandOutput { status, stdout, stderr: stderr.to_string() })
                })
                .collect(),
            calls: Mutex::new(Vec::new()),
        }
    }

    #[test]
    fn pull_resolves_digest_id_and_config_in_order() {
        let runner = owned_runner(full_pull_script("docker.io/library/metube@sha256:aaaa"));
        let pulled = pull_and_inspect_image(&runner, BuildTool::Podman, "ghcr.io/alexta69/metube:latest").unwrap();
        assert_eq!(pulled.resolved_digest, "docker.io/library/metube@sha256:aaaa");
        assert_eq!(pulled.final_image_digest, "sha256:0123abcd"); // bare hex normalized
        assert_eq!(pulled.image_config.cmd, vec!["node", "server.js"]);
        assert_eq!(pulled.image_config.exposed_tcp_ports, vec![3000, 8080]);

        let calls = runner.calls();
        let idx = |p: &str| calls.iter().position(|c| c.starts_with(p)).unwrap_or_else(|| panic!("missing {p}: {calls:?}"));
        // Pull precedes every inspect; the pull is platform-pinned.
        assert!(calls[0].starts_with("podman pull --platform linux/amd64 ghcr.io/alexta69/metube:latest"), "{:?}", calls[0]);
        assert!(idx("podman pull") < idx("podman image inspect --format {{index .RepoDigests 0}}"));
    }

    #[test]
    fn pull_accepts_a_digest_ref_and_round_trips_the_digest() {
        // A digest-pinned ref resolves to itself (RepoDigests echoes the manifest digest).
        let runner = owned_runner(full_pull_script("ghcr.io/alexta69/metube@sha256:beef"));
        let pulled =
            pull_and_inspect_image(&runner, BuildTool::Podman, "ghcr.io/alexta69/metube@sha256:beef").unwrap();
        assert_eq!(pulled.resolved_digest, "ghcr.io/alexta69/metube@sha256:beef");
    }

    #[test]
    fn pull_undigestable_image_fails_closed() {
        let mut script = full_pull_script("docker.io/library/metube@sha256:aaaa");
        script[1] = ("podman image inspect --format {{index .RepoDigests 0}}", 0, "\n".to_string(), "");
        let runner = owned_runner(script);
        let err = pull_and_inspect_image(&runner, BuildTool::Podman, "metube:local").unwrap_err();
        assert!(err.contains("did not resolve to a registry digest"), "{err}");
    }

    #[test]
    fn pull_failure_surfaces_stderr_tail() {
        let mut script = full_pull_script("docker.io/library/metube@sha256:aaaa");
        script[0] = ("podman pull", 125, String::new(), "Error: initializing source: manifest unknown");
        let runner = owned_runner(script);
        let err = pull_and_inspect_image(&runner, BuildTool::Podman, "ghcr.io/nope/nope:latest").unwrap_err();
        assert!(err.contains("pull image") && err.contains("manifest unknown"), "{err}");
    }
}
