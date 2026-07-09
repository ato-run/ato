//! Parse-only round-trip + eligibility-validator coverage for the Ready-State
//! authoring tables.

use super::*;
use crate::foundation::types::manifest::CapsuleManifest;

/// A minimal v0.3 manifest body that parses today; tests append Ready-State
/// tables to it.
const BASE: &str = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
run = "python app.py"
port = 8080

[targets.app.readiness_probe]
type = "http"
path = "/health"
"#;

fn parse(extra: &str) -> CapsuleManifest {
    let toml = format!("{BASE}\n{extra}");
    CapsuleManifest::from_toml(&toml).expect("manifest should parse")
}

#[test]
fn base_manifest_has_no_ready_state_tables() {
    let m = parse("");
    assert!(m.snapshot.is_none());
    assert!(m.secrets.is_empty());
    assert!(m.bindings.is_empty());
    assert!(m.external.is_empty());
    assert!(m.context.is_none());
    assert!(!m.is_ready_state_eligible());
    // Defaulted snapshot config is the legacy cold path.
    assert_eq!(m.snapshot_config().mode, SnapshotMode::None);
}

#[test]
fn snapshot_table_parses_with_defaults() {
    let m = parse(
        r#"
[snapshot]
mode = "warm"
"#,
    );
    let snap = m.snapshot.clone().expect("snapshot present");
    assert_eq!(snap.mode, SnapshotMode::Warm);
    // Defaults applied for omitted fields.
    assert_eq!(snap.boot_until, BootUntil::Healthcheck);
    assert!(snap.sanitize_after_restore);
    assert_eq!(snap.runner_class, None);
    assert_eq!(snap.max_restore_seconds, None);
    assert!(m.is_ready_state_eligible());
}

#[test]
fn snapshot_table_full() {
    let m = parse(
        r#"
[snapshot]
mode = "booted"
boot_until = "first_request"
sanitize_after_restore = false
runner_class = "managed/linux-aarch64"
max_restore_seconds = 8
"#,
    );
    let snap = m.snapshot.clone().unwrap();
    assert_eq!(snap.mode, SnapshotMode::Booted);
    assert_eq!(snap.boot_until, BootUntil::FirstRequest);
    assert!(!snap.sanitize_after_restore);
    assert_eq!(snap.runner_class.as_deref(), Some("managed/linux-aarch64"));
    assert_eq!(snap.max_restore_seconds, Some(8));
}

#[test]
fn secrets_bindings_external_context_parse() {
    let m = parse(
        r#"
[secrets.openai_api_key]
required = true
env = "OPENAI_API_KEY"
delivery = "proxy"
class = "api_key"

[bindings.user_files]
kind = "user_files"
scope = "user"
mount = "/data"

[external.llm]
type = "llm"
providers = ["local", "ato_model"]
provision = "parallel"
locality = "local_preferred"
degraded = "demo"

[context]
store = "user_shared"
artifacts = true
index = true
mount = "/context"
provenance = true
"#,
    );

    let secret = m.secrets.get("openai_api_key").expect("secret present");
    assert!(secret.required);
    assert_eq!(secret.env.as_deref(), Some("OPENAI_API_KEY"));
    assert_eq!(secret.delivery, SecretDelivery::Proxy);
    assert_eq!(secret.class, SecretClass::ApiKey);

    let binding = m.bindings.get("user_files").expect("binding present");
    assert_eq!(binding.kind, BindingKind::UserFiles);
    assert_eq!(binding.scope, BindingScope::User);
    assert_eq!(binding.mount.as_deref(), Some("/data"));

    let ext = m.external.get("llm").expect("external present");
    assert_eq!(ext.kind, "llm");
    assert_eq!(ext.providers, vec!["local", "ato_model"]);
    assert_eq!(ext.provision, ProvisionMode::Parallel);
    assert_eq!(ext.locality, Locality::LocalPreferred);
    assert_eq!(ext.degraded, DegradedMode::Demo);

    let ctx = m.context.clone().expect("context present");
    assert_eq!(ctx.store, ContextStore::UserShared);
    assert!(ctx.artifacts && ctx.index && ctx.provenance);
    assert_eq!(ctx.mount.as_deref(), Some("/context"));
}

/// Old binaries ignore unknown tables; new binaries ignore unknown *keys*
/// inside our tables (no `deny_unknown_fields`). Confirm an extra key does not
/// break parsing.
#[test]
fn unknown_key_inside_ready_state_table_is_tolerated() {
    let m = parse(
        r#"
[snapshot]
mode = "warm"
future_field = "ignored"
"#,
    );
    assert_eq!(m.snapshot.unwrap().mode, SnapshotMode::Warm);
}

/// A capsule that declares none of the Ready-State tables must serialize back
/// without emitting empty tables (skip_serializing_if), preserving the
/// round-trip for legacy recipes.
#[test]
fn empty_ready_state_tables_are_not_serialized() {
    let m = parse("");
    let json = m.to_json().expect("to_json");
    assert!(!json.contains("\"snapshot\""), "snapshot should be skipped: {json}");
    assert!(!json.contains("\"secrets\""), "secrets should be skipped");
    assert!(!json.contains("\"bindings\""), "bindings should be skipped");
    assert!(!json.contains("\"external\""), "external should be skipped");
    assert!(!json.contains("\"context\""), "context should be skipped");
}

/// JSON round-trip of a populated manifest preserves the Ready-State tables.
#[test]
fn json_round_trip_preserves_tables() {
    let original = parse(
        r#"
[snapshot]
mode = "warm"

[secrets.tok]
required = false
env = "TOK"

[external.vec]
type = "service"
"#,
    );
    let json = original.to_json().expect("to_json");
    let back = CapsuleManifest::from_json(&json).expect("from_json");
    assert_eq!(back.snapshot, original.snapshot);
    assert_eq!(back.secrets, original.secrets);
    assert_eq!(back.external, original.external);
}

/// The shipped sample recipe must parse, declare the Ready-State tables, and
/// stay Public-Instant-Run eligible. Keeps `samples/ready-state-demo` honest.
#[test]
fn shipped_sample_recipe_parses_and_is_eligible() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples/ready-state-demo/capsule.toml");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let m = CapsuleManifest::from_toml(&text).expect("sample should parse");

    assert!(m.is_ready_state_eligible());
    assert_eq!(m.snapshot_config().mode, SnapshotMode::Warm);
    assert!(m.secrets.contains_key("openai_api_key"));
    assert!(m.bindings.contains_key("user_files"));
    assert!(m.external.contains_key("llm"));
    assert!(m.context.is_some());

    let e = m.instant_run_eligibility();
    assert!(e.eligible, "sample must stay eligible: {:?}", e.blocking_reasons);
}

// ── eligibility validator ─────────────────────────────────────────────────

#[test]
fn eligible_minimal_capsule() {
    // BASE has a readiness probe, no network/state/secrets/external.
    let m = parse("");
    let e = m.instant_run_eligibility();
    assert!(e.eligible, "should be eligible: {:?}", e.blocking_reasons);
    assert!(e.network_default_deny);
    assert!(e.ephemeral_only);
    assert!(e.no_secrets_required);
    assert_eq!(e.external_count, 0);
    assert!(e.has_healthcheck);
}

#[test]
fn ineligible_required_secret() {
    let m = parse(
        r#"
[secrets.key]
required = true
env = "KEY"
"#,
    );
    let e = m.instant_run_eligibility();
    assert!(!e.eligible);
    assert!(!e.no_secrets_required);
    assert!(e.blocking_reasons.iter().any(|r| r.contains("secrets.key")));
}

#[test]
fn optional_secret_stays_eligible() {
    let m = parse(
        r#"
[secrets.key]
required = false
env = "KEY"
"#,
    );
    assert!(m.instant_run_eligibility().eligible);
}

#[test]
fn ineligible_persistent_state() {
    let m = parse(
        r#"
[state.db]
kind = "filesystem"
durability = "persistent"
purpose = "store"
"#,
    );
    let e = m.instant_run_eligibility();
    assert!(!e.eligible);
    assert!(!e.ephemeral_only);
    assert!(e.blocking_reasons.iter().any(|r| r.contains("state.db")));
}

#[test]
fn ephemeral_state_stays_eligible() {
    let m = parse(
        r#"
[state.scratch]
kind = "filesystem"
durability = "ephemeral"
purpose = "cache"
"#,
    );
    assert!(m.instant_run_eligibility().ephemeral_only);
}

#[test]
fn ineligible_too_many_external() {
    let m = parse(
        r#"
[external.a]
type = "llm"
[external.b]
type = "service"
[external.c]
type = "browser_worker"
[external.d]
type = "service"
"#,
    );
    let e = m.instant_run_eligibility();
    assert_eq!(e.external_count, 4);
    assert!(!e.external_within_limit);
    assert!(!e.eligible);
}

#[test]
fn three_external_is_within_limit() {
    let m = parse(
        r#"
[external.a]
type = "llm"
[external.b]
type = "service"
[external.c]
type = "browser_worker"
"#,
    );
    let e = m.instant_run_eligibility();
    assert_eq!(e.external_count, MAX_EXTERNAL_CAPABILITIES);
    assert!(e.external_within_limit);
    assert!(e.eligible);
}

#[test]
fn ineligible_unrestricted_network() {
    let m = parse(
        r#"
[requirements.capabilities]
network = "bidirectional"
"#,
    );
    let e = m.instant_run_eligibility();
    assert!(!e.network_default_deny);
    assert!(!e.eligible);
}

#[test]
fn egress_with_allowlist_is_default_deny() {
    let m = parse(
        r#"
[requirements.capabilities]
network = "egress"

[network]
egress_allow = ["api.openai.com"]
"#,
    );
    let e = m.instant_run_eligibility();
    assert!(e.network_default_deny);
    assert!(e.eligible);
}

#[test]
fn missing_healthcheck_is_blocking() {
    // Build a manifest with no readiness probe at all.
    let toml = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
run = "python app.py"
"#;
    let m = CapsuleManifest::from_toml(toml).expect("parse");
    let e = m.instant_run_eligibility();
    assert!(!e.has_healthcheck);
    assert!(!e.eligible);
}

#[test]
fn probe_on_non_default_target_does_not_count() {
    // The default target has NO readiness probe; a different target does. The
    // run serves the default target, so it is NOT ready-detectable → ineligible.
    let toml = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
run = "python app.py"

[targets.other]
runtime = "source"
run = "python other.py"

[targets.other.readiness_probe]
type = "http"
path = "/health"
"#;
    let m = CapsuleManifest::from_toml(toml).expect("parse");
    let e = m.instant_run_eligibility();
    assert!(
        !e.has_healthcheck,
        "a probe on a non-serving target must not satisfy the healthcheck gate"
    );
    assert!(!e.eligible);
    assert!(e.blocking_reasons.iter().any(|r| r.contains("serving target 'app'")));
}

#[test]
fn probe_on_default_target_counts() {
    // BASE already puts the probe on the default target `app`.
    let m = parse("");
    assert!(m.instant_run_eligibility().has_healthcheck);
}

#[test]
fn probe_on_a_service_in_the_default_target_graph_counts() {
    // The default target has no probe of its own, but a service it depends on
    // (transitively) does. That service IS in the serving graph → counts.
    let toml = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "web"

[targets.web]
runtime = "source"
run = "python app.py"

[services.web]
target = "web"
depends_on = ["db"]

[services.db]
entrypoint = "postgres"

[services.db.readiness_probe]
tcp_connect = "localhost"
port = "PORT"
"#;
    let m = CapsuleManifest::from_toml(toml).expect("parse");
    let e = m.instant_run_eligibility();
    assert!(
        e.has_healthcheck,
        "a probe on a service in the serving target's graph must count: {:?}",
        e.blocking_reasons
    );
}

#[test]
fn probe_on_a_service_bound_to_another_target_does_not_count() {
    // The serving (default) target `web` has no probe; a service explicitly
    // bound to a DIFFERENT target carries one. Out of the serving graph → no.
    let toml = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "web"

[targets.web]
runtime = "source"
run = "python app.py"

[targets.worker]
runtime = "source"
run = "python worker.py"

[services.worker]
target = "worker"

[services.worker.readiness_probe]
tcp_connect = "localhost"
port = "PORT"
"#;
    let m = CapsuleManifest::from_toml(toml).expect("parse");
    let e = m.instant_run_eligibility();
    assert!(
        !e.has_healthcheck,
        "a probe on a service bound to a non-serving target must not count"
    );
    assert!(!e.eligible);
}

// ── GPU judgment (Workstream C) ───────────────────────────────────────────

#[test]
fn gpu_vram_min_blocks_ready_state() {
    let m = parse(
        r#"
[requirements]
vram_min = "8GB"
"#,
    );
    let e = m.instant_run_eligibility();
    assert!(e.requires_gpu);
    assert!(e.gpu_blocks_ready_state);
    assert!(!e.gpu_external_binding);
    assert!(!e.eligible);
    assert!(
        e.blocking_reasons
            .iter()
            .any(|r| r.contains("GPU") && r.contains("snapshot")),
        "{:?}",
        e.blocking_reasons
    );
}

#[test]
fn gpu_vram_recommended_blocks_ready_state() {
    let m = parse(
        r#"
[requirements]
vram_recommended = "8GB"
"#,
    );
    let e = m.instant_run_eligibility();
    assert!(e.gpu_blocks_ready_state);
    assert!(!e.eligible);
}

#[test]
fn gpu_build_flag_blocks_ready_state() {
    let m = parse(
        r#"
[build]
gpu = true
"#,
    );
    let e = m.instant_run_eligibility();
    assert!(e.requires_gpu);
    assert!(e.gpu_blocks_ready_state);
    assert!(!e.eligible);
}

#[test]
fn gpu_external_capability_stays_eligible() {
    let m = parse(
        r#"
[external.gpu]
type = "gpu"
"#,
    );
    let e = m.instant_run_eligibility();
    assert!(e.requires_gpu);
    assert!(e.gpu_external_binding);
    assert!(!e.gpu_blocks_ready_state);
    assert!(e.eligible, "{:?}", e.blocking_reasons);
}

#[test]
fn gpu_vram_with_external_binding_stays_eligible() {
    let m = parse(
        r#"
[requirements]
vram_min = "8GB"

[external.gpu]
type = "gpu"
"#,
    );
    let e = m.instant_run_eligibility();
    assert!(e.requires_gpu);
    assert!(e.gpu_external_binding);
    assert!(!e.gpu_blocks_ready_state);
    assert!(e.eligible, "{:?}", e.blocking_reasons);
}

#[test]
fn cpu_only_capsule_has_no_gpu_requirement() {
    let e = parse("").instant_run_eligibility();
    assert!(!e.requires_gpu);
    assert!(!e.gpu_external_binding);
    assert!(!e.gpu_blocks_ready_state);
    assert!(e.eligible);
}

#[test]
fn gpu_mode_classification_matrix() {
    use crate::foundation::types::ready_state::GpuMode;
    assert_eq!(parse("").gpu_mode(), GpuMode::None);
    assert_eq!(
        parse("[requirements]\nvram_min = \"8GB\"").gpu_mode(),
        GpuMode::Passthrough
    );
    assert_eq!(
        parse("[external.gpu]\ntype = \"gpu\"").gpu_mode(),
        GpuMode::External
    );
    assert_eq!(
        parse("[requirements]\nvram_min = \"8GB\"\n[external.gpu]\ntype = \"gpu\"").gpu_mode(),
        GpuMode::External
    );
}

#[test]
fn gpu_predicate_is_additive() {
    let m = parse("[requirements]\nvram_min = \"8GB\"");
    let e = m.instant_run_eligibility();
    assert!(
        e.network_default_deny && e.ephemeral_only && e.no_secrets_required && e.has_healthcheck
    );
    assert_eq!(e.blocking_reasons.len(), 1);
    assert!(e.blocking_reasons[0].contains("GPU"));
}

#[test]
fn accelerator_external_kind_recognised() {
    let m = parse(
        r#"
[external.accel]
type = "accelerator"
"#,
    );
    assert!(m.has_external_gpu_capability());
    assert!(m.instant_run_eligibility().gpu_external_binding);
}

fn read_sample(rel: &str) -> CapsuleManifest {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    CapsuleManifest::from_toml(&text).expect("sample should parse")
}

#[test]
fn shipped_gpu_external_sample_is_eligible() {
    let m = read_sample("../../samples/ready-state-gpu-external/capsule.toml");
    use crate::foundation::types::ready_state::GpuMode;
    assert_eq!(m.gpu_mode(), GpuMode::External);
    let e = m.instant_run_eligibility();
    assert!(e.requires_gpu && e.gpu_external_binding && !e.gpu_blocks_ready_state);
    assert!(e.eligible, "{:?}", e.blocking_reasons);
}

#[test]
fn shipped_gpu_invm_sample_is_ineligible_fail_closed() {
    let m = read_sample("../../samples/ready-state-gpu-invm/capsule.toml");
    use crate::foundation::types::ready_state::GpuMode;
    assert_eq!(m.gpu_mode(), GpuMode::Passthrough);
    let e = m.instant_run_eligibility();
    assert!(e.requires_gpu && e.gpu_blocks_ready_state && !e.eligible);
    // The manifest itself is still valid (parsed fine); only Ready-State is gated.
    assert!(m.is_ready_state_eligible(), "snapshot mode is warm");
}

#[test]
fn v12_secret_description_and_binding_mode_parse() {
    let m = parse(
        r#"
[secrets.OPENAI_API_KEY]
required = true
description = "OpenAI API key used to summarize documents"
env = "OPENAI_API_KEY"

[bindings.input_files]
kind = "user_files"
mode = "read_only"
mount = "/input"
required = true

[bindings.output_dir]
kind = "user_files"
mode = "read_write"
mount = "/output"

[bindings.appdata]
kind = "state"
mount = "/data"
"#,
    );

    let secret = m.secrets.get("OPENAI_API_KEY").expect("secret present");
    assert_eq!(
        secret.description.as_deref(),
        Some("OpenAI API key used to summarize documents")
    );

    let input = m.bindings.get("input_files").expect("input binding");
    assert_eq!(input.mode, Some(BindingMode::ReadOnly));
    let output = m.bindings.get("output_dir").expect("output binding");
    assert_eq!(output.mode, Some(BindingMode::ReadWrite));
    // Absent mode stays None — the kind's default is a launch-time decision,
    // never guessed at parse time.
    let state = m.bindings.get("appdata").expect("state binding");
    assert_eq!(state.mode, None);
    // description is optional and absent stays None.
    assert!(m.secrets.get("OPENAI_API_KEY").unwrap().required);
}

#[test]
fn v12_unknown_binding_mode_is_rejected() {
    // An invalid mode must be a parse error, not a silent None — "rw" is not
    // in the contract vocabulary (read_only | read_write). Control: the same
    // manifest with a valid mode parses, so the failure is the mode alone.
    let good = "[bindings.input]\nkind = \"user_files\"\nmode = \"read_only\"\n";
    let bad = "[bindings.input]\nkind = \"user_files\"\nmode = \"rw\"\n";
    assert!(CapsuleManifest::from_toml(&format!("{BASE}\n{good}")).is_ok());
    assert!(CapsuleManifest::from_toml(&format!("{BASE}\n{bad}")).is_err());
}
