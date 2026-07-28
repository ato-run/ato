//! `ato runner setup --official-preview` — prepare a fixed-IP host as an
//! OFFICIAL preview runner behind Caddy (ato#1006 ingress PR C).
//!
//! The ato-admin console provisions the runner's managed ingress (hierarchical
//! hostname family + grey-cloud DNS A records) and hands the operator its
//! public base URL, e.g. `https://runner-abc.runner.ato.run`. This mode makes
//! the BOX side match that contract:
//!
//!   - Caddy terminates public HTTPS per slot hostname (`<base>` +
//!     `s<N>.<base>`) and proxies each to its LOOPBACK slot port
//!     (`127.0.0.1:8420 + N`) — the Caddyfile is generated here from the same
//!     hostname scheme the control plane registered, so the two sides cannot
//!     drift.
//!   - `ato runner serve` keeps its loopback-only default bind. A runner unit
//!     that passes a non-loopback `--proxy-listen` is flagged and rewritten:
//!     a publicly bound slot port would let clients bypass Caddy/TLS AND the
//!     control plane's slot-hostname allowlist entirely.
//!   - /etc/ato/runner.env gains (append-only) `ATO_RUNNER_PREVIEW=1`,
//!     `ATO_RUNNER_PUBLIC_BASE_URL=<base>`, `ATO_RUNNER_MAX_SLOTS=<n>` — the
//!     systemd service reads all three; PREVIEW=1 is what makes the runner
//!     advertise `restore_snapshot_preview` (once KVM/Firecracker are ready).
//!
//! Pure logic only in this module (validation, planning, rendering, parsing);
//! host probes return `Check`s and the mutation runs through setup.rs's
//! plan/confirm/apply flow.
//!
//! ## Generations (wizard hold ingress)
//!
//! The wizard hold routes and the preview routes are ONE generation, not two
//! files that happen to be written together. Activating them separately can
//! leave Caddy serving a preview fragment from one input and a wizard fragment
//! from another — a mixed generation nobody authored, which is exactly the
//! state that is hardest to reason about when an origin misbehaves.
//!
//! ```text
//! generated/
//!   generations/<digest>/
//!     preview.caddy
//!     wizard-hold.caddy
//!   current -> generations/<digest>
//! ```
//!
//! Caddy includes `current/*.caddy` only, so the swap of one symlink is the
//! whole activation. [`render_generation`] renders the set and
//! [`generation_digest`] names it.
//!
//! **Dead-code allow (module-scoped):** the plan/generation half below has no
//! production caller until the activation step lands (generation directory,
//! `caddy validate`, atomic symlink swap, reload with rollback, loopback probe,
//! ato-api reconciliation). It is exercised in full by this module's tests. The
//! allow is removed with that step.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Result, bail};

use super::{Check, checks};

/// Where the generated Caddyfile goes unless the operator overrides it.
pub(crate) const DEFAULT_CADDYFILE_PATH: &str = "/etc/caddy/Caddyfile";
pub(crate) const CADDY_UNIT: &str = "caddy.service";
/// First loopback slot port — must match `runner_agent::DEFAULT_PROXY_LISTEN`
/// (asserted in tests) and the control plane's Caddyfile generator.
pub(crate) const LOCAL_BASE_PORT: u16 = 8420;
/// The only host a slot port may bind. See [`parse_loopback_listen`].
pub(crate) const LOOPBACK: &str = "127.0.0.1";
/// The path Caddy answers 200 on for every ingress vhost, independently of the
/// slot app behind it — the same constant the ato-admin validate probes.
pub(crate) const WELLKNOWN_PATH: &str = "/.well-known/ato-runner-ingress";

/// Official-preview inputs, validated before any plan is derived.
#[derive(Debug, Clone)]
pub(crate) struct OfficialPreviewConfig {
    /// e.g. `https://runner-abc.runner.ato.run` — the ato-managed base the
    /// admin console provisioned. Its host anchors the whole hostname family.
    pub public_base_url: String,
    pub max_slots: usize,
    pub caddyfile_path: String,
    /// `127.0.0.1:<port>` — the FIRST slot's builder hold-proxy listen address.
    /// Later slots take consecutive ports, exactly as the preview family does.
    ///
    /// `None` ⇒ this runner serves no wizard holds: no wizard origin is
    /// generated and no ingress slot is registered. That is the same
    /// all-or-nothing switch the builder itself uses — a wizard origin with no
    /// builder behind it is a route that can only 502.
    pub hold_proxy_listen: Option<String>,
}

/// The base hostname extracted from a VALIDATED public base URL.
pub(crate) fn base_hostname(public_base_url: &str) -> String {
    public_base_url
        .trim_start_matches("https://")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

/// Validate the managed public base URL:
///   - `https://<hostname>` EXACTLY — no port (Caddy owns 443), no path, no
///     query/fragment, no userinfo;
///   - hostname is a DNS name, not an IP literal / loopback (the control plane
///     blocks raw-IP upstreams — Cloudflare error 1003);
///   - env-file-safe (it is written into /etc/ato/runner.env, so whitespace or
///     control characters would be env-line injection).
pub(crate) fn validate_public_base_url(url: &str) -> Result<()> {
    let Some(rest) = url.strip_prefix("https://") else {
        bail!("--public-base-url must be https:// (got {url:?}) — Caddy terminates TLS on 443");
    };
    let host = rest.trim_end_matches('/');
    if host.is_empty()
        || rest.matches('/').count() > 1
        || (rest.contains('/') && !rest.ends_with('/'))
    {
        bail!("--public-base-url must be exactly https://<hostname> with no path (got {url:?})");
    }
    if host.contains(':') || host.contains('@') || host.contains('?') || host.contains('#') {
        bail!(
            "--public-base-url must not contain a port, userinfo, query, or fragment (got {url:?})"
        );
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        bail!("--public-base-url must not contain whitespace or control characters");
    }
    let h = host.to_ascii_lowercase();
    if h == "localhost" || h.starts_with("127.") {
        bail!("--public-base-url must not be loopback (got {url:?})");
    }
    let is_ipv4 = h.split('.').count() == 4 && h.split('.').all(|o| o.parse::<u8>().is_ok());
    if is_ipv4 {
        bail!(
            "--public-base-url must be a DNS hostname, not an IP literal (a Cloudflare Worker cannot fetch a raw-IP upstream — error 1003)"
        );
    }
    if !h.contains('.')
        || !h
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
    {
        bail!(
            "--public-base-url hostname may only contain [a-z0-9.-] and must be a dotted DNS name (got {url:?})"
        );
    }
    // DNS label boundaries: no empty label (leading/trailing/double dot), no
    // leading/trailing hyphen per label, ≤63 chars per label.
    for label in h.split('.') {
        if label.is_empty() || label.len() > 63 || label.starts_with('-') || label.ends_with('-') {
            bail!("--public-base-url hostname has an invalid DNS label {label:?} (got {url:?})");
        }
    }
    Ok(())
}

pub(crate) fn validate_config(cfg: &OfficialPreviewConfig) -> Result<()> {
    validate_public_base_url(&cfg.public_base_url)?;
    if !(1..=64).contains(&cfg.max_slots) {
        bail!("--max-slots must be in [1, 64] (got {})", cfg.max_slots);
    }
    if !cfg.caddyfile_path.starts_with('/') || cfg.caddyfile_path.contains("..") {
        bail!(
            "--caddyfile must be a plain absolute path (got {:?})",
            cfg.caddyfile_path
        );
    }
    Ok(())
}

/// `(hostname, loopback port)` per slot: `s0.<base>` → 8420, `s1.<base>` → 8421…
///
/// Kept as a thin projection of [`derive_slot_plans`] so the preview hostname
/// and port scheme has ONE definition. See that function's doc for why.
pub(crate) fn slot_hostnames(base_host: &str, max_slots: usize) -> Vec<(String, u16)> {
    (0..max_slots)
        .map(|i| (format!("s{i}.{base_host}"), preview_port(i)))
        .collect()
}

fn preview_port(slot_index: usize) -> u16 {
    LOCAL_BASE_PORT + slot_index as u16
}

/// The wizard hold-ingress hostname for a slot: `w0.<base>`, `w1.<base>`…
///
/// A separate label from the preview `s<N>` family on purpose. The two front
/// DIFFERENT processes — `s<N>` reaches the runner agent's slot proxy, `w<N>`
/// reaches a builder-resident held guest — and a shared label would make a
/// stale route from one silently answer for the other.
fn wizard_hostname(base_host: &str, slot_index: usize) -> String {
    format!("w{slot_index}.{base_host}")
}

/// One slot, resolved once: every port, hostname and identifier that the
/// systemd arguments, the Caddy routes and the ato-api ingress registration all
/// have to agree on.
///
/// This exists because those three consumers previously computed the same
/// numbers independently. That is a drift waiting to happen: the day one of
/// them changes its base port or its hostname scheme, the other two keep
/// working — pointing at the wrong place — and the failure surfaces as a 502 on
/// someone else's preview, far from the edit. Deriving all three from one value
/// makes disagreement impossible rather than unlikely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunnerSlotPlan {
    /// Stable builder identity, as ato-api's ingress registry keys it.
    pub builder_id: String,
    /// Slot identity within that builder.
    pub slot_id: String,
    /// Public origin hostname for the runner preview (`s<N>.<base>`).
    pub preview_origin: String,
    /// Loopback address the preview origin proxies to.
    pub preview_listen: String,
    /// Public origin hostname for the wizard hold (`w<N>.<base>`), when this
    /// daemon serves holds at all.
    pub wizard_origin: Option<String>,
    /// Loopback address the wizard origin proxies to — the builder's
    /// `--hold-proxy-listen` for this slot.
    pub hold_proxy_listen: Option<String>,
}

/// Derive every slot's plan, or refuse.
///
/// Deterministic in its inputs: the same config yields byte-identical plans, so
/// the generated files are byte-identical too (a re-run that changed nothing
/// must not churn the active generation).
///
/// Refuses BEFORE anything is written when preview and hold ports would
/// collide. A collision discovered after the files are on disk is discovered by
/// Caddy, at reload, with one origin already pointing at the wrong process.
pub(crate) fn derive_slot_plans(cfg: &OfficialPreviewConfig) -> Result<Vec<RunnerSlotPlan>> {
    let base_host = base_hostname(&cfg.public_base_url);
    let builder_id = base_host.clone();

    let hold_base_port = match cfg.hold_proxy_listen.as_deref() {
        None => None,
        Some(listen) => Some(parse_loopback_listen(listen)?.1),
    };

    let mut plans = Vec::with_capacity(cfg.max_slots);
    for index in 0..cfg.max_slots {
        let preview = preview_port(index);
        let hold = hold_base_port
            .map(|base_port| {
                base_port.checked_add(index as u16).ok_or_else(|| {
                    anyhow::anyhow!(
                        "--hold-proxy-listen base port {base_port} + {index} slots overflows"
                    )
                })
            })
            .transpose()?;
        plans.push(RunnerSlotPlan {
            builder_id: builder_id.clone(),
            slot_id: format!("s{index}"),
            preview_origin: format!("s{index}.{base_host}"),
            preview_listen: format!("{LOOPBACK}:{preview}"),
            wizard_origin: hold.map(|_| wizard_hostname(&base_host, index)),
            hold_proxy_listen: hold.map(|port| format!("{LOOPBACK}:{port}")),
        });
    }
    reject_port_collisions(&plans)?;
    reject_origin_problems(&plans)?;
    Ok(plans)
}

/// Every origin this plan set claims must be a legal DNS name and must be
/// claimed exactly once.
///
/// Checked on the PLAN, not inside the renderer: an origin is an identity that
/// systemd arguments, Caddy routes and the ato-api registration all carry, so a
/// name that is only rejected at render time would already have been registered
/// by one of the other two. `s<N>` and `w<N>` are also checked against each
/// other here — they are different families precisely so a stale route from one
/// cannot answer for the other, and that only holds if they never collide.
fn reject_origin_problems(plans: &[RunnerSlotPlan]) -> Result<()> {
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for plan in plans {
        for (origin, family) in [
            (Some(&plan.preview_origin), "preview"),
            (plan.wizard_origin.as_ref(), "wizard"),
        ] {
            let Some(origin) = origin else { continue };
            validate_origin_hostname(origin)?;
            let owner = format!("{family} {}", plan.slot_id);
            if let Some(previous) = seen.insert(origin.clone(), owner.clone()) {
                bail!(
                    "origin {origin} is claimed by both {previous} and {owner} — two routes for \
                     one hostname means whichever Caddy loads last silently answers for the other"
                );
            }
        }
    }
    Ok(())
}

/// The DNS limits an origin must satisfy to be resolvable at all: ≤253 bytes
/// overall, ≤63 per label, no empty label, no leading/trailing hyphen.
///
/// The base hostname is already validated on the way in; this catches what
/// PREFIXING it can break — a base that is legal on its own can exceed 253 once
/// `w<N>.` is added, and the slot label itself must be legal.
fn validate_origin_hostname(origin: &str) -> Result<()> {
    if origin.len() > 253 {
        bail!(
            "generated origin {origin:?} is {} bytes, over the 253-byte DNS limit — \
             shorten --public-base-url",
            origin.len()
        );
    }
    for label in origin.split('.') {
        if label.is_empty() || label.len() > 63 || label.starts_with('-') || label.ends_with('-') {
            bail!("generated origin {origin:?} has an invalid DNS label {label:?}");
        }
    }
    Ok(())
}

/// Every loopback port this plan set claims must be claimed exactly once —
/// across BOTH families, and including the base-host vhost that shares slot 0's
/// preview port.
fn reject_port_collisions(plans: &[RunnerSlotPlan]) -> Result<()> {
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for plan in plans {
        for (listen, owner) in [
            (
                Some(&plan.preview_listen),
                format!("preview {}", plan.slot_id),
            ),
            (
                plan.hold_proxy_listen.as_ref(),
                format!("hold {}", plan.slot_id),
            ),
        ] {
            let Some(listen) = listen else { continue };
            if let Some(previous) = seen.insert(listen.clone(), owner.clone()) {
                bail!(
                    "listen address {listen} is claimed by both {previous} and {owner} — \
                     preview and hold ports must not overlap (adjust --hold-proxy-listen \
                     so its range clears {LOOPBACK}:{LOCAL_BASE_PORT}+max_slots)"
                );
            }
        }
    }
    Ok(())
}

/// `host:port` restricted to the loopback host, because a publicly bound slot
/// port would let clients bypass Caddy, TLS, and the control plane's
/// slot-hostname allowlist entirely — the same reason `--proxy-listen` is
/// rewritten when it is non-loopback.
fn parse_loopback_listen(listen: &str) -> Result<(String, u16)> {
    let (host, port) = listen
        .rsplit_once(':')
        .ok_or_else(|| anyhow::anyhow!("--hold-proxy-listen must be host:port (got {listen:?})"))?;
    if host != LOOPBACK {
        bail!(
            "--hold-proxy-listen must bind {LOOPBACK} (got {host:?}) — a publicly bound hold port \
             would let clients reach a held guest without Caddy, TLS, or the slot allowlist"
        );
    }
    let port: u16 = port
        .parse()
        .map_err(|_| anyhow::anyhow!("--hold-proxy-listen port is not a u16 (got {listen:?})"))?;
    if port == 0 {
        bail!("--hold-proxy-listen port must not be 0 (got {listen:?})");
    }
    Ok((host.to_string(), port))
}

/// Render the Caddyfile for this runner's hostname family. Format-compatible
/// with the ato-admin console's generator (the console shows the same file):
/// every vhost answers the well-known probe 200 REGARDLESS of slot app state,
/// then reverse-proxies to its loopback slot port. The base hostname serves
/// the runner root proxy (slot 0's port).
pub(crate) fn render_caddyfile(base_host: &str, max_slots: usize) -> String {
    let vhost = |hostname: &str, port: u16| {
        format!(
            "{hostname} {{\n\
             \thandle {WELLKNOWN_PATH} {{\n\
             \t\trespond \"ok\" 200\n\
             \t}}\n\
             \treverse_proxy 127.0.0.1:{port}\n\
             }}\n"
        )
    };
    let mut blocks = vec![format!(
        "# ato official-preview runner ingress — generated by `ato runner setup --official-preview`.\n\
         # Do not hand-edit; re-run setup after changing max_slots.\n"
    )];
    blocks.push(vhost(base_host, LOCAL_BASE_PORT));
    for (hostname, port) in slot_hostnames(base_host, max_slots) {
        blocks.push(vhost(&hostname, port));
    }
    blocks.join("\n")
}

/// One generated Caddy fragment: its file name inside a generation directory,
/// and its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GeneratedFragment {
    pub file_name: &'static str,
    pub content: String,
}

pub(crate) const PREVIEW_FRAGMENT: &str = "preview.caddy";
pub(crate) const WIZARD_FRAGMENT: &str = "wizard-hold.caddy";
/// Domain separator for the generation manifest digest.
pub(crate) const GENERATION_DOMAIN: &str = "ato.runner-caddy-generation/v1";
/// The path every generated vhost answers with its own generation identity.
///
/// Served by the CADDY ROUTE, never by the app behind it: the question a probe
/// asks is "which generation is answering for this origin", and an app cannot
/// answer that — it does not know, and it would still answer after a rollback.
pub(crate) const GENERATION_MARKER_PATH: &str = "/.well-known/ato/ingress-generation";
/// Stands in for the marker body while the generation is being hashed.
///
/// The identity is derived FROM the rendered routes, so a body containing the
/// identity cannot also be an input to it. Hashing this fixed token instead
/// breaks the circle: the digest commits the ROUTES, and the marker is a pure
/// function of that digest, so committing it would add nothing and could not
/// terminate.
const MARKER_PLACEHOLDER: &str = "__ATO_GENERATION_MARKER__";

fn vhost(hostname: &str, listen: &str) -> String {
    format!(
        "{hostname} {{\n\
         \thandle {WELLKNOWN_PATH} {{\n\
         \t\trespond \"ok\" 200\n\
         \t}}\n\
         \thandle {GENERATION_MARKER_PATH} {{\n\
         \t\theader Content-Type application/json\n\
         \t\trespond \"{MARKER_PLACEHOLDER}\" 200\n\
         \t}}\n\
         \treverse_proxy {listen}\n\
         }}\n"
    )
}

/// A generation's identity as a probe reads it back.
///
/// `id` is the short handle (and the directory name); `digest` is the full
/// commitment. Both are checked, because a probe that matched only the
/// truncated handle would accept a generation that merely collided with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GenerationIdentity {
    pub id: String,
    pub digest: String,
}

impl GenerationIdentity {
    /// The exact JSON body the marker route returns. Serialized from a fixed
    /// field order rather than a map, so the bytes a probe compares are the
    /// bytes this produced.
    pub(crate) fn marker_body(&self) -> String {
        format!(
            "{{\"schema\":\"{GENERATION_DOMAIN}\",\"generation_id\":\"{}\",\"generation_digest\":\"{}\"}}",
            self.id, self.digest
        )
    }
}

fn escape_caddy_quoted_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Substitute the real marker into a rendered generation.
///
/// Called AFTER the identity is derived, for the reason
/// [`MARKER_PLACEHOLDER`] exists. The bytes that reach disk therefore differ
/// from the bytes that were hashed — by exactly the marker, which the digest
/// determines — so the manifest records the FINAL lengths while the id records
/// the routes.
pub(crate) fn finalize_generation(
    fragments: &[GeneratedFragment],
    identity: &GenerationIdentity,
) -> Vec<GeneratedFragment> {
    let body = escape_caddy_quoted_string(&identity.marker_body());
    fragments
        .iter()
        .map(|fragment| GeneratedFragment {
            file_name: fragment.file_name,
            content: fragment.content.replace(MARKER_PLACEHOLDER, &body),
        })
        .collect()
}

/// The identity of a rendered generation: the short handle plus the full
/// commitment.
pub(crate) fn generation_identity(fragments: &[GeneratedFragment]) -> GenerationIdentity {
    let full = generation_manifest_hash(fragments);
    GenerationIdentity {
        id: full[..16].to_string(),
        digest: full,
    }
}

/// Render the whole generation from the plans.
///
/// Both fragments come from the SAME plan set, in one call, because they are
/// one generation: rendering them independently is what makes it possible to
/// activate a preview fragment from one input and a wizard fragment from
/// another. The caller activates the returned set as a unit (see the module
/// doc on generations).
///
/// The wizard fragment is emitted ONLY when at least one slot has a hold proxy.
/// An empty file would be indistinguishable, at a glance, from a generation
/// that failed to render one — and it would make "this runner serves holds" a
/// property of file contents rather than of the plan.
pub(crate) fn render_generation(
    plans: &[RunnerSlotPlan],
    base_host: &str,
) -> Vec<GeneratedFragment> {
    let header = |what: &str| {
        format!(
            "# ato {what} — generated by `ato runner setup --official-preview`.\n\
             # Do not hand-edit: this file is replaced wholesale on the next run,\n\
             # and a hand-added route would be dropped while its ato-api ingress\n\
             # registration survived.\n"
        )
    };

    let mut preview = vec![header("official-preview runner ingress")];
    // The base host serves slot 0's port, as it always has.
    preview.push(vhost(base_host, &format!("{LOOPBACK}:{LOCAL_BASE_PORT}")));
    for plan in plans {
        preview.push(vhost(&plan.preview_origin, &plan.preview_listen));
    }

    let mut fragments = vec![GeneratedFragment {
        file_name: PREVIEW_FRAGMENT,
        content: preview.join("\n"),
    }];

    let wizard: Vec<String> = plans
        .iter()
        .filter_map(|plan| {
            let origin = plan.wizard_origin.as_ref()?;
            let listen = plan.hold_proxy_listen.as_ref()?;
            Some(vhost(origin, listen))
        })
        .collect();
    if !wizard.is_empty() {
        let mut blocks = vec![header("submission-wizard hold ingress")];
        blocks.extend(wizard);
        fragments.push(GeneratedFragment {
            file_name: WIZARD_FRAGMENT,
            content: blocks.join("\n"),
        });
    }
    fragments
}

/// Every fragment name a generation can contain, in a FIXED order.
///
/// The digest below walks this list rather than the fragments that happen to be
/// present, which is what lets "no wizard fragment" be a distinct value from
/// "an empty wizard fragment".
pub(crate) const GENERATION_FRAGMENTS: &[&str] = &[PREVIEW_FRAGMENT, WIZARD_FRAGMENT];

/// The identity of a generation: a digest over a domain-separated MANIFEST of
/// its fragments, not over their concatenated bytes.
///
/// Concatenation cannot tell three different generations apart:
///
/// - a missing `wizard-hold.caddy` from a present-but-empty one;
/// - two fragments whose bytes differ only in where one ends and the next
///   begins (`"ab" + "c"` vs `"a" + "bc"`);
/// - the same bytes filed under a different name.
///
/// All three are different things to Caddy, so all three must be different
/// generations. The manifest therefore commits, per KNOWN fragment name and in
/// a fixed order, either its length and content digest or an explicit absence
/// marker — never nothing at all.
///
/// Used as the generation directory name and recorded alongside the ato-api
/// registration, so "which routes are live" is answerable without diffing
/// files. Taken over the OUTPUT so that two configs which render identically
/// share one generation: a re-run that changes nothing must not swap the active
/// generation.
pub(crate) fn generation_digest(fragments: &[GeneratedFragment]) -> String {
    generation_manifest_hash(fragments)[..16].to_string()
}

/// The full manifest hash. [`generation_digest`] is its short handle.
fn generation_manifest_hash(fragments: &[GeneratedFragment]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(GENERATION_DOMAIN.as_bytes());
    hasher.update(&[0]);
    for name in GENERATION_FRAGMENTS {
        hasher.update(name.as_bytes());
        hasher.update(&[0]);
        match fragments.iter().find(|f| f.file_name == *name) {
            Some(fragment) => {
                hasher.update(b"present");
                hasher.update(&[0]);
                hasher.update(&(fragment.content.len() as u64).to_le_bytes());
                hasher.update(blake3::hash(fragment.content.as_bytes()).as_bytes());
            }
            None => {
                hasher.update(b"absent");
                hasher.update(&[0]);
            }
        }
    }
    hasher.finalize().to_hex().to_string()
}

/// The ingress slots ato-api should have registered for this runner — the
/// DESIRED state, not a diff.
///
/// Only slots that actually have a wizard route appear. A registration for a
/// slot with no route is the failure this whole shape exists to prevent: the
/// api hands out a preview URL, and the origin 502s because nothing on the box
/// answers for it.
pub(crate) fn desired_ingress_slots(plans: &[RunnerSlotPlan]) -> Vec<&RunnerSlotPlan> {
    plans
        .iter()
        .filter(|plan| plan.wizard_origin.is_some() && plan.hold_proxy_listen.is_some())
        .collect()
}

/// The env-file lines official-preview needs, minus keys `existing` already
/// defines (append-only — an operator-set line is NEVER rewritten). Returns
/// `(missing_lines, conflicts)`; a conflict is an existing key whose value
/// disagrees with what this mode needs, surfaced as an explicit warning
/// because appending cannot fix it.
pub(crate) fn official_env_lines(
    existing: &BTreeMap<String, String>,
    cfg: &OfficialPreviewConfig,
) -> (Vec<String>, Vec<String>) {
    let base_host = base_hostname(&cfg.public_base_url);
    let wanted: Vec<(&str, String)> = vec![
        ("ATO_RUNNER_PREVIEW", "1".to_string()),
        (
            "ATO_RUNNER_PUBLIC_BASE_URL",
            cfg.public_base_url.trim_end_matches('/').to_string(),
        ),
        ("ATO_RUNNER_MAX_SLOTS", cfg.max_slots.to_string()),
        // Per-slot READY URLs. Without this, serve (a) refuses to start at
        // max_slots >= 2 (validate_multi_slot_public_url requires a template)
        // and (b) at max_slots = 1 reports slot 0 ready at the BASE host —
        // which the control plane's slot-hostname allowlist (s0.<base>…)
        // rejects at /ready. The template renders exactly the s{N}.<base>
        // hostnames the admin console registered.
        (
            "ATO_RUNNER_PUBLIC_URL_TEMPLATE",
            format!("https://s{{slot}}.{base_host}"),
        ),
    ];
    let mut lines = Vec::new();
    let mut conflicts = Vec::new();
    for (k, v) in wanted {
        match existing.get(k) {
            None => lines.push(format!("{k}={v}")),
            Some(cur) if cur.trim_end_matches('/') != v.trim_end_matches('/') => {
                conflicts.push(format!(
                    "{k} is already set to {cur:?} (this mode needs {v:?}) — {ENV_FILE} is append-only; edit the line manually",
                    ENV_FILE = super::ENV_FILE,
                ));
            }
            Some(_) => {}
        }
    }
    (lines, conflicts)
}

/// True when a systemd unit's text binds the runner proxy publicly — an
/// ExecStart passing `--proxy-listen` with a host other than 127.0.0.1 /
/// localhost. A publicly bound slot port bypasses Caddy, TLS, and the
/// control plane's slot-hostname allowlist. Pure.
pub(crate) fn unit_has_public_proxy_listen(unit_text: &str) -> bool {
    for line in unit_text.lines() {
        let Some(idx) = line.find("--proxy-listen") else {
            continue;
        };
        let rest = line[idx + "--proxy-listen".len()..].trim_start_matches(['=', ' ']);
        let value = rest.split_whitespace().next().unwrap_or("");
        let host = value.rsplit_once(':').map(|(h, _)| h).unwrap_or(value);
        let h = host.trim_matches(['[', ']']).to_ascii_lowercase();
        if !(h.is_empty() || h == "127.0.0.1" || h == "localhost" || h == "::1") {
            return true;
        }
    }
    false
}

/// Parse `/proc/net/tcp`-format text for LISTEN (st == 0A) sockets on `port`.
/// Pure — the probe reads /proc/net/tcp{,6} and feeds the text here.
pub(crate) fn proc_net_tcp_listens_on(text: &str, port: u16) -> bool {
    text.lines().skip(1).any(|line| {
        // Columns: sl local_address rem_address st …
        let mut cols = line.split_whitespace();
        let local = cols.nth(1).unwrap_or("");
        let st = cols.nth(1).unwrap_or("");
        st.eq_ignore_ascii_case("0A")
            && local
                .rsplit_once(':')
                .and_then(|(_, p)| u16::from_str_radix(p, 16).ok())
                == Some(port)
    })
}

fn host_listens_on(port: u16) -> bool {
    ["/proc/net/tcp", "/proc/net/tcp6"].iter().any(|path| {
        std::fs::read_to_string(path)
            .map(|t| proc_net_tcp_listens_on(&t, port))
            .unwrap_or(false)
    })
}

fn caddy_service_state() -> Option<String> {
    let out = std::process::Command::new("systemctl")
        .args(["is-active", CADDY_UNIT])
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Official-preview host checks, appended to the base `gather()` set. Check
/// ids here drive the extra plan actions in setup.rs.
pub(crate) fn gather(cfg: &OfficialPreviewConfig) -> Vec<Check> {
    let mut out = Vec::new();
    let base_host = base_hostname(&cfg.public_base_url);

    // Caddy binary.
    out.push(
        match std::process::Command::new("caddy").arg("version").output() {
            Ok(o) if o.status.success() => Check::ok(
                "caddy",
                "Caddy",
                String::from_utf8_lossy(&o.stdout).trim().to_string(),
            ),
            _ => Check::missing(
                "caddy",
                "Caddy",
                "not installed",
                "apt-get install -y caddy",
            ),
        },
    );

    // Caddyfile content: ours iff it names the base hostname (regenerating is
    // idempotent; a foreign file is backed up before being replaced).
    out.push(match std::fs::read_to_string(&cfg.caddyfile_path) {
        Ok(text) if text.contains(&base_host) && text.contains(WELLKNOWN_PATH) => Check::ok(
            "caddyfile",
            "Caddyfile",
            format!("{} serves {base_host}", cfg.caddyfile_path),
        ),
        Ok(_) => Check::missing(
            "caddyfile",
            "Caddyfile",
            format!(
                "{} exists but does not serve {base_host}",
                cfg.caddyfile_path
            ),
            "setup --fix regenerates it (existing file backed up)",
        ),
        Err(_) => Check::missing(
            "caddyfile",
            "Caddyfile",
            format!("{} absent", cfg.caddyfile_path),
            "setup --fix writes it",
        ),
    });

    // Caddy service state.
    out.push(match caddy_service_state().as_deref() {
        Some("active") => Check::ok("caddy_service", "Caddy service", "active"),
        Some(state) => Check::missing(
            "caddy_service",
            "Caddy service",
            format!(
                "installed, {}",
                if state.is_empty() { "inactive" } else { state }
            ),
            "systemctl enable --now caddy",
        ),
        None => Check::missing(
            "caddy_service",
            "Caddy service",
            "systemctl unavailable or caddy not installed",
            "install caddy, then systemctl enable --now caddy",
        ),
    });

    // Ports 80/443: Caddy needs both (443 TLS + 80 for the ACME HTTP challenge
    // and the https redirect). Occupied-by-caddy is the healthy end state.
    let caddy_active = caddy_service_state().as_deref() == Some("active");
    for port in [80u16, 443] {
        let id: &'static str = if port == 80 { "port_80" } else { "port_443" };
        out.push(if host_listens_on(port) {
            if caddy_active {
                Check::ok(
                    id,
                    "ingress port",
                    format!("{port} in LISTEN (caddy active)"),
                )
            } else {
                Check::warn(
                    id,
                    "ingress port",
                    format!(
                        "{port} is in LISTEN but caddy is not active — another server owns it?"
                    ),
                    "stop the conflicting service; Caddy must bind 80+443",
                )
            }
        } else {
            Check::ok(
                id,
                "ingress port",
                format!("{port} free (Caddy will bind it)"),
            )
        });
    }

    // Runner unit must not bind the slot proxy publicly.
    let unit_path = Path::new(super::SYSTEMD_DIR).join(super::RUNNER_UNIT);
    if let Ok(text) = std::fs::read_to_string(&unit_path) {
        out.push(if unit_has_public_proxy_listen(&text) {
            Check::missing(
                "unit_runner_loopback",
                "runner proxy bind",
                format!(
                    "{} passes a PUBLIC --proxy-listen — slot ports must stay on 127.0.0.1 (a public bind bypasses Caddy/TLS and the slot-hostname allowlist)",
                    super::RUNNER_UNIT
                ),
                "setup --fix rewrites the unit (loopback default)",
            )
        } else {
            Check::ok("unit_runner_loopback", "runner proxy bind", "loopback only")
        });
    }

    // Official env keys (append-only fix; conflicts surface separately).
    let env_vals = std::fs::read_to_string(super::ENV_FILE)
        .map(|t| checks::env_file_values(&t))
        .unwrap_or_default();
    let (missing, _conflicts) = official_env_lines(&env_vals, cfg);
    out.push(if missing.is_empty() {
        Check::ok(
            "env_official",
            "official-preview env",
            "ATO_RUNNER_PREVIEW / PUBLIC_BASE_URL / MAX_SLOTS / PUBLIC_URL_TEMPLATE configured",
        )
    } else {
        Check::missing(
            "env_official",
            "official-preview env",
            format!("missing: {}", missing.join(", ")),
            "setup --fix appends them",
        )
    });

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> OfficialPreviewConfig {
        OfficialPreviewConfig {
            public_base_url: "https://runner-abc.runner.ato.run".into(),
            max_slots: 2,
            caddyfile_path: DEFAULT_CADDYFILE_PATH.into(),
            hold_proxy_listen: None,
        }
    }

    fn cfg_with_holds() -> OfficialPreviewConfig {
        OfficialPreviewConfig {
            hold_proxy_listen: Some("127.0.0.1:8500".into()),
            ..cfg()
        }
    }

    fn fragment<'a>(
        fragments: &'a [GeneratedFragment],
        name: &str,
    ) -> Option<&'a GeneratedFragment> {
        fragments.iter().find(|f| f.file_name == name)
    }

    /// No `--hold-proxy-listen` ⇒ no wizard route and nothing to register.
    ///
    /// This is the same all-or-nothing switch the builder itself uses. A wizard
    /// origin generated for a runner that serves no holds is a route that can
    /// only 502, and an ingress registration behind it is worse: the api hands
    /// out a preview URL for it.
    #[test]
    fn without_a_hold_proxy_there_is_no_wizard_route_and_nothing_to_register() {
        let plans = derive_slot_plans(&cfg()).expect("plans");
        assert!(plans.iter().all(|p| p.wizard_origin.is_none()));
        assert!(plans.iter().all(|p| p.hold_proxy_listen.is_none()));

        let fragments = render_generation(&plans, &base_hostname(&cfg().public_base_url));
        assert!(
            fragment(&fragments, WIZARD_FRAGMENT).is_none(),
            "a runner that serves no holds must not emit a wizard fragment at all — \
             not even an empty one"
        );
        assert!(desired_ingress_slots(&plans).is_empty());
    }

    /// With a hold proxy, every slot gets BOTH families, and the ports are
    /// derived — never hard-coded.
    #[test]
    fn a_hold_proxy_yields_one_wizard_origin_per_slot_with_derived_ports() {
        let plans = derive_slot_plans(&cfg_with_holds()).expect("plans");
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].preview_origin, "s0.runner-abc.runner.ato.run");
        assert_eq!(plans[0].preview_listen, "127.0.0.1:8420");
        assert_eq!(
            plans[0].wizard_origin.as_deref(),
            Some("w0.runner-abc.runner.ato.run")
        );
        assert_eq!(
            plans[0].hold_proxy_listen.as_deref(),
            Some("127.0.0.1:8500")
        );
        // Consecutive, by the same rule the preview family uses.
        assert_eq!(plans[1].preview_listen, "127.0.0.1:8421");
        assert_eq!(
            plans[1].hold_proxy_listen.as_deref(),
            Some("127.0.0.1:8501")
        );
        // The identity ato-api keys its ingress registry by.
        assert_eq!(plans[1].builder_id, "runner-abc.runner.ato.run");
        assert_eq!(plans[1].slot_id, "s1");
        assert_eq!(desired_ingress_slots(&plans).len(), 2);
    }

    /// A preview/hold port overlap is refused while it is still only a plan.
    ///
    /// Discovered after the files are written, the same collision is discovered
    /// by Caddy at reload — with one origin already pointing at the wrong
    /// process. The check therefore runs on the plan, before anything is
    /// rendered, let alone activated.
    #[test]
    fn a_preview_hold_port_collision_is_refused_before_anything_is_rendered() {
        let colliding = OfficialPreviewConfig {
            max_slots: 4,
            // 8421 is slot 1's PREVIEW port.
            hold_proxy_listen: Some("127.0.0.1:8421".into()),
            ..cfg()
        };
        let error = derive_slot_plans(&colliding).expect_err("must refuse");
        let message = format!("{error:#}");
        assert!(message.contains("claimed by both"), "{message}");
        assert!(message.contains("127.0.0.1:8421"), "{message}");
    }

    /// A hold proxy bound anywhere but loopback is refused.
    ///
    /// A publicly bound hold port reaches a held guest without Caddy, TLS, or
    /// the control plane's slot-hostname allowlist — the same reason a
    /// non-loopback `--proxy-listen` is rewritten.
    #[test]
    fn a_non_loopback_hold_proxy_is_refused() {
        for listen in ["0.0.0.0:8500", "65.109.37.38:8500", "8500", "127.0.0.1:0"] {
            let error = derive_slot_plans(&OfficialPreviewConfig {
                hold_proxy_listen: Some(listen.into()),
                ..cfg()
            })
            .expect_err("must refuse {listen}");
            assert!(!format!("{error:#}").is_empty());
        }
    }

    /// The same input renders byte-identical output, and therefore the same
    /// generation digest.
    ///
    /// This is what makes a no-op re-run a no-op: the activation step compares
    /// digests, so any nondeterminism here (map iteration order, a timestamp in
    /// a header) would swap the active generation and reload Caddy on every
    /// single run.
    #[test]
    fn the_same_input_renders_byte_identical_generations() {
        let base = base_hostname(&cfg_with_holds().public_base_url);
        let first = render_generation(&derive_slot_plans(&cfg_with_holds()).unwrap(), &base);
        let second = render_generation(&derive_slot_plans(&cfg_with_holds()).unwrap(), &base);
        assert_eq!(first, second);
        assert_eq!(generation_digest(&first), generation_digest(&second));

        // And a DIFFERENT input must not share the digest, or activation could
        // skip a swap it needed to make.
        let narrower = render_generation(
            &derive_slot_plans(&OfficialPreviewConfig {
                max_slots: 1,
                ..cfg_with_holds()
            })
            .unwrap(),
            &base,
        );
        assert_ne!(generation_digest(&first), generation_digest(&narrower));
    }

    /// Shrinking `max_slots` drops the slot from BOTH the routes and the
    /// desired registration set — the two must never disagree about which
    /// slots exist.
    #[test]
    fn shrinking_max_slots_drops_the_slot_from_routes_and_from_desired_registrations() {
        let base = base_hostname(&cfg_with_holds().public_base_url);
        let wide = derive_slot_plans(&cfg_with_holds()).unwrap();
        assert_eq!(desired_ingress_slots(&wide).len(), 2);

        let narrow = derive_slot_plans(&OfficialPreviewConfig {
            max_slots: 1,
            ..cfg_with_holds()
        })
        .unwrap();
        assert_eq!(desired_ingress_slots(&narrow).len(), 1);
        let fragments = render_generation(&narrow, &base);
        let wizard = fragment(&fragments, WIZARD_FRAGMENT).expect("wizard fragment");
        assert!(wizard.content.contains("w0.runner-abc.runner.ato.run"));
        assert!(
            !wizard.content.contains("w1."),
            "the dropped slot must leave no route behind: {}",
            wizard.content
        );
    }

    /// Absence, emptiness and a different byte boundary are three DIFFERENT
    /// generations.
    ///
    /// A digest over concatenated fragment bytes cannot tell them apart, and all
    /// three are different things to Caddy — the first serves no wizard routes,
    /// the second serves a file that parses to none, and the third is a
    /// different set of routes entirely.
    #[test]
    fn the_generation_digest_separates_absence_from_emptiness_and_from_a_moved_boundary() {
        let preview = |content: &str| GeneratedFragment {
            file_name: PREVIEW_FRAGMENT,
            content: content.to_string(),
        };
        let wizard = |content: &str| GeneratedFragment {
            file_name: WIZARD_FRAGMENT,
            content: content.to_string(),
        };

        let absent = generation_digest(&[preview("a")]);
        let empty = generation_digest(&[preview("a"), wizard("")]);
        assert_ne!(
            absent, empty,
            "no wizard fragment must not digest the same as an empty one"
        );

        // Same concatenated bytes, different boundary.
        let left = generation_digest(&[preview("ab"), wizard("c")]);
        let right = generation_digest(&[preview("a"), wizard("bc")]);
        assert_ne!(left, right, "the fragment boundary is identity-bearing");

        // Same bytes under a different name.
        let swapped = generation_digest(&[preview("c"), wizard("ab")]);
        assert_ne!(left, swapped, "the fragment NAME is identity-bearing");
    }

    /// The marker is answered by the Caddy ROUTE, on every origin, and its body
    /// is substituted only after the identity exists.
    ///
    /// An app cannot answer "which generation is serving me" — it does not know,
    /// and it would keep answering the same thing after a rollback. So the
    /// question has to be answered by the thing that was actually swapped.
    #[test]
    fn every_origin_answers_the_generation_marker_from_the_route() {
        let plans = derive_slot_plans(&cfg_with_holds()).unwrap();
        let base = base_hostname(&cfg_with_holds().public_base_url);
        let rendered = render_generation(&plans, &base);
        let identity = generation_identity(&rendered);
        let finalized = finalize_generation(&rendered, &identity);

        for fragment in &finalized {
            assert!(
                !fragment.content.contains(MARKER_PLACEHOLDER),
                "the placeholder must not survive into the published bytes"
            );
        }
        let preview = fragment(&finalized, PREVIEW_FRAGMENT).unwrap();
        let wizard = fragment(&finalized, WIZARD_FRAGMENT).unwrap();
        // One marker handler per vhost: base + s0 + s1 in preview, w0 + w1 in
        // wizard. A generation that answered on only some origins would let a
        // probe pass while another origin still served the old routes.
        assert_eq!(preview.content.matches(GENERATION_MARKER_PATH).count(), 3);
        assert_eq!(wizard.content.matches(GENERATION_MARKER_PATH).count(), 2);
        let caddy_marker = escape_caddy_quoted_string(&identity.marker_body());
        assert!(preview.content.contains(&caddy_marker));
        assert!(wizard.content.contains(&caddy_marker));
        assert!(
            !preview
                .content
                .contains(&format!("respond \"{}\" 200", identity.marker_body())),
            "raw JSON quotes would terminate the Caddy string"
        );
    }

    /// The identity is derived from the routes, and substituting the marker
    /// does not change it — that is what makes the derivation terminate.
    #[test]
    fn the_identity_commits_the_routes_not_the_marker() {
        let plans = derive_slot_plans(&cfg_with_holds()).unwrap();
        let base = base_hostname(&cfg_with_holds().public_base_url);
        let rendered = render_generation(&plans, &base);
        let identity = generation_identity(&rendered);

        assert_eq!(identity.id, generation_digest(&rendered));
        assert_eq!(identity.digest.len(), 64, "the full commitment is kept");
        assert!(identity.digest.starts_with(&identity.id));

        // Re-deriving from the finalized bytes would be a different value — and
        // that is precisely why the marker is substituted afterwards rather than
        // hashed.
        let finalized = finalize_generation(&rendered, &identity);
        assert_ne!(generation_digest(&finalized), identity.id);

        // Different routes, different identity.
        let narrower = render_generation(
            &derive_slot_plans(&OfficialPreviewConfig {
                max_slots: 1,
                ..cfg_with_holds()
            })
            .unwrap(),
            &base,
        );
        assert_ne!(generation_identity(&narrower).digest, identity.digest);
    }

    /// A base hostname that is legal on its own can become illegal once a slot
    /// label is prefixed — caught on the plan, before any consumer has taken
    /// the name.
    #[test]
    fn an_origin_that_prefixing_makes_illegal_is_refused_on_the_plan() {
        let long_label = "a".repeat(60);
        let base = format!("https://{long_label}.{long_label}.{long_label}.{long_label}.ato.run");
        let error = derive_slot_plans(&OfficialPreviewConfig {
            public_base_url: base,
            ..cfg_with_holds()
        })
        .expect_err("must refuse");
        assert!(
            format!("{error:#}").contains("253-byte DNS limit"),
            "{error:#}"
        );
    }

    /// The preview family keeps rendering exactly as it did, so an existing
    /// runner's active routes do not move when this feature merely becomes
    /// available.
    #[test]
    fn the_preview_fragment_is_unchanged_for_a_runner_with_no_holds() {
        let plans = derive_slot_plans(&cfg()).unwrap();
        let base = base_hostname(&cfg().public_base_url);
        let fragments = render_generation(&plans, &base);
        let preview = fragment(&fragments, PREVIEW_FRAGMENT).expect("preview fragment");
        for expected in [
            "runner-abc.runner.ato.run {",
            "s0.runner-abc.runner.ato.run {",
            "s1.runner-abc.runner.ato.run {",
            "reverse_proxy 127.0.0.1:8420",
            "reverse_proxy 127.0.0.1:8421",
            WELLKNOWN_PATH,
        ] {
            assert!(preview.content.contains(expected), "missing {expected}");
        }
    }

    #[test]
    fn local_base_port_matches_the_serve_default() {
        // The Caddyfile upstreams and `ato runner serve`'s default bind must
        // agree, or every slot proxies into the void.
        assert_eq!(
            format!("127.0.0.1:{LOCAL_BASE_PORT}"),
            crate::application::runner_agent::DEFAULT_PROXY_LISTEN
        );
    }

    #[test]
    fn public_base_url_validation() {
        assert!(validate_public_base_url("https://runner-abc.runner.ato.run").is_ok());
        assert!(validate_public_base_url("https://runner-abc.runner.ato.run/").is_ok());
        for bad in [
            "http://runner-abc.runner.ato.run",       // https only
            "https://runner-abc.runner.ato.run:8421", // no port — Caddy owns 443
            "https://runner-abc.runner.ato.run/app",  // no path
            "https://65.109.37.38",                   // raw IP
            "https://127.0.0.1",                      // loopback
            "https://localhost",                      // loopback
            "https://runner",                         // not a dotted DNS name
            "https://runner abc.run",                 // whitespace ⇒ env injection
            "https://a.run\nATO_X=1",                 // newline ⇒ env injection
            "https://user@host.run",                  // userinfo
            "https://-bad.runner.ato.run",            // leading hyphen label
            "https://bad-.runner.ato.run",            // trailing hyphen label
            "https://bad..runner.ato.run",            // empty label
            "https://.runner.ato.run",                // empty leading label
        ] {
            assert!(
                validate_public_base_url(bad).is_err(),
                "must reject {bad:?}"
            );
        }
    }

    #[test]
    fn caddyfile_covers_base_and_every_slot_with_the_wellknown_handler() {
        let text = render_caddyfile("runner-abc.runner.ato.run", 2);
        // base + s0 + s1 vhosts, each with the well-known handler.
        assert_eq!(text.matches(WELLKNOWN_PATH).count(), 3);
        assert!(text.contains("runner-abc.runner.ato.run {"));
        assert!(text.contains("s0.runner-abc.runner.ato.run {"));
        assert!(text.contains("s1.runner-abc.runner.ato.run {"));
        assert!(!text.contains("s2."), "no vhost beyond max_slots");
        // Upstreams are LOOPBACK slot ports only.
        assert!(text.contains("reverse_proxy 127.0.0.1:8420"));
        assert!(text.contains("reverse_proxy 127.0.0.1:8421"));
        assert!(!text.contains("0.0.0.0"));
    }

    #[test]
    fn env_lines_are_append_only_and_conflicts_surface() {
        // Fresh file: all four keys appended — INCLUDING the per-slot URL
        // template, without which serve refuses multi-slot startup and slot 0
        // would report the base host (rejected by the slot allowlist).
        let (lines, conflicts) = official_env_lines(&BTreeMap::new(), &cfg());
        assert_eq!(
            lines,
            vec![
                "ATO_RUNNER_PREVIEW=1",
                "ATO_RUNNER_PUBLIC_BASE_URL=https://runner-abc.runner.ato.run",
                "ATO_RUNNER_MAX_SLOTS=2",
                "ATO_RUNNER_PUBLIC_URL_TEMPLATE=https://s{slot}.runner-abc.runner.ato.run",
            ]
        );
        assert!(conflicts.is_empty());

        // Matching existing values: nothing appended, no conflicts (trailing
        // slash tolerated).
        let mut existing = BTreeMap::new();
        existing.insert("ATO_RUNNER_PREVIEW".into(), "1".into());
        existing.insert(
            "ATO_RUNNER_PUBLIC_BASE_URL".into(),
            "https://runner-abc.runner.ato.run/".into(),
        );
        existing.insert("ATO_RUNNER_MAX_SLOTS".into(), "2".into());
        existing.insert(
            "ATO_RUNNER_PUBLIC_URL_TEMPLATE".into(),
            "https://s{slot}.runner-abc.runner.ato.run".into(),
        );
        let (lines, conflicts) = official_env_lines(&existing, &cfg());
        assert!(lines.is_empty() && conflicts.is_empty());

        // Disagreeing operator values: NOT rewritten, surfaced as conflicts.
        existing.insert(
            "ATO_RUNNER_PUBLIC_BASE_URL".into(),
            "https://old.example.com".into(),
        );
        existing.insert(
            "ATO_RUNNER_PUBLIC_URL_TEMPLATE".into(),
            "https://runner.example.com:{port}/".into(),
        );
        let (lines, conflicts) = official_env_lines(&existing, &cfg());
        assert!(lines.is_empty());
        assert_eq!(conflicts.len(), 2);
        assert!(
            conflicts
                .iter()
                .any(|c| c.contains("ATO_RUNNER_PUBLIC_BASE_URL"))
        );
        assert!(
            conflicts
                .iter()
                .any(|c| c.contains("ATO_RUNNER_PUBLIC_URL_TEMPLATE"))
        );
    }

    #[test]
    fn template_satisfies_serve_startup_validation_at_multi_slot() {
        // The exact template setup writes must pass BOTH serve-side gates, or
        // the systemd service crash-loops at max_slots >= 2.
        let (lines, _) = official_env_lines(&BTreeMap::new(), &cfg());
        let template = lines
            .iter()
            .find_map(|l| l.strip_prefix("ATO_RUNNER_PUBLIC_URL_TEMPLATE="))
            .expect("template line present");
        crate::application::runner_agent::validate_public_url_template(Some(template))
            .expect("template must carry a {slot}/{port} placeholder");
        crate::application::runner_agent::validate_multi_slot_public_url(2, Some(template))
            .expect("multi-slot serve must accept the official-preview env");
    }

    #[test]
    fn template_renders_slot_hostnames_never_the_base_host() {
        let (lines, _) = official_env_lines(&BTreeMap::new(), &cfg());
        let template = lines
            .iter()
            .find_map(|l| l.strip_prefix("ATO_RUNNER_PUBLIC_URL_TEMPLATE="))
            .unwrap();
        // Slot i serves on LOCAL_BASE_PORT + i; the rendered ready URL must be
        // the s{N} slot hostname the control-plane allowlist registered.
        let s0 = crate::application::runner_agent::render_public_url_template(
            template,
            LOCAL_BASE_PORT,
            0,
        );
        let s1 = crate::application::runner_agent::render_public_url_template(
            template,
            LOCAL_BASE_PORT + 1,
            1,
        );
        assert_eq!(s0, "https://s0.runner-abc.runner.ato.run");
        assert_eq!(s1, "https://s1.runner-abc.runner.ato.run");
        // Never the bare base host (the allowlist only carries slot hostnames).
        for url in [&s0, &s1] {
            assert!(
                !url.starts_with("https://runner-abc."),
                "must not render the base host: {url}"
            );
        }
    }

    #[test]
    fn public_proxy_listen_detection() {
        // The unit setup writes (no flags) is loopback-safe.
        assert!(!unit_has_public_proxy_listen(
            &super::super::setup::render_unit(super::super::RUNNER_UNIT)
        ));
        for public in [
            "ExecStart=/usr/local/bin/ato runner serve --proxy-listen 0.0.0.0:8420",
            "ExecStart=/usr/local/bin/ato runner serve --proxy-listen=10.0.0.5:8420",
            "ExecStart=ato runner serve --proxy-listen [::]:8420",
        ] {
            assert!(unit_has_public_proxy_listen(public), "must flag {public:?}");
        }
        for loopback in [
            "ExecStart=/usr/local/bin/ato runner serve",
            "ExecStart=ato runner serve --proxy-listen 127.0.0.1:8420",
            "ExecStart=ato runner serve --proxy-listen=localhost:9000",
        ] {
            assert!(
                !unit_has_public_proxy_listen(loopback),
                "must pass {loopback:?}"
            );
        }
    }

    #[test]
    fn proc_net_tcp_listen_parse() {
        // 0050 = 80, 01BB = 443; st 0A = LISTEN.
        let tcp = "  sl  local_address rem_address   st\n\
                   0: 00000000:0050 00000000:0000 0A\n\
                   1: 0100007F:20E4 00000000:0000 0A\n\
                   2: 00000000:01BB 00000000:0000 01\n";
        assert!(proc_net_tcp_listens_on(tcp, 80));
        assert!(proc_net_tcp_listens_on(tcp, 8420)); // 0x20E4
        assert!(
            !proc_net_tcp_listens_on(tcp, 443),
            "st 01 (ESTABLISHED) is not LISTEN"
        );
        assert!(!proc_net_tcp_listens_on(tcp, 9999));
    }

    #[test]
    fn config_validation_bounds() {
        assert!(validate_config(&cfg()).is_ok());
        let mut c = cfg();
        c.max_slots = 0;
        assert!(validate_config(&c).is_err());
        c.max_slots = 65;
        assert!(validate_config(&c).is_err());
        c.max_slots = 1;
        c.caddyfile_path = "relative/Caddyfile".into();
        assert!(validate_config(&c).is_err());
        c.caddyfile_path = "/etc/caddy/../evil".into();
        assert!(validate_config(&c).is_err());
    }
}
