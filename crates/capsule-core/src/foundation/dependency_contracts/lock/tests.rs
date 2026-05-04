//! Lock-time verifier tests. Each verification rule from RFC §9.1 has at
//! least one happy + one failure-path test.

use std::collections::BTreeMap;

use super::{verify_and_lock, DependencyLockInput, ResolvedProviderManifest};
use crate::foundation::dependency_contracts::LockError;
use crate::foundation::types::CapsuleManifest;

// Use explicit `[targets.<X>]` block form so v0.3 normalizer does not
// fold the manifest top-level `required_env` into the target table.
// The verifier resolves `{{env.X}}` against the manifest top-level
// `required_env` per RFC §5.2.
const HAPPY_CONSUMER: &str = r#"
schema_version = "0.3"
name = "demo-consumer"
version = "0.1.0"
type = "app"
default_target = "app"
required_env = ["PG_PASSWORD"]

[targets.app]
runtime = "source"
driver = "python"
runtime_version = "3.11"
run = "main.py"
needs = ["db"]

[dependencies.db]
capsule = "capsule://ato/postgres@16"
contract = "service@1"

[dependencies.db.parameters]
database = "appdb"

[dependencies.db.credentials]
password = "{{env.PG_PASSWORD}}"

[dependencies.db.state]
name = "data"
"#;

const HAPPY_PROVIDER: &str = r#"
schema_version = "0.3"
name = "postgres"
version = "16.4"
type = "app"
default_target = "server"

[targets.server]
runtime = "source"
driver = "native"
run = "postgres -D {{state.dir}}"
port = 5432

[contracts."service@1"]
target = "server"
ready = { type = "probe", run = "pg_isready", timeout = "30s" }

[contracts."service@1".parameters]
database = { type = "string", required = true }

[contracts."service@1".credentials]
password = { type = "string", required = true }

[contracts."service@1".identity_exports]
database = "{{params.database}}"
protocol = "postgresql"
major = "16"

[contracts."service@1".runtime_exports]
PGPORT = "{{port}}"

[contracts."service@1".runtime_exports.DATABASE_URL]
value = "postgresql://postgres:{{credentials.password}}@{{host}}:{{port}}/{{params.database}}"
secret = true

[contracts."service@1".state]
required = true
version = "16"
"#;

fn parse(text: &str) -> CapsuleManifest {
    CapsuleManifest::from_toml(text).expect("manifest")
}

fn provider(text: &str) -> ResolvedProviderManifest {
    ResolvedProviderManifest {
        requested: "capsule://ato/postgres@16".to_string(),
        resolved: "capsule://ato/postgres@sha256:abc123def".to_string(),
        manifest: parse(text),
    }
}

fn happy_input<'a>(consumer: &'a CapsuleManifest) -> DependencyLockInput<'a> {
    let mut providers = BTreeMap::new();
    providers.insert("db".to_string(), provider(HAPPY_PROVIDER));
    DependencyLockInput {
        consumer,
        providers,
    }
}

#[test]
fn happy_path_emits_lock_with_all_invariants() {
    let consumer = parse(HAPPY_CONSUMER);
    let lock = verify_and_lock(happy_input(&consumer)).expect("verify");

    let entry = lock.entries.get("db").expect("db entry");
    assert_eq!(entry.requested, "capsule://ato/postgres@16");
    assert_eq!(entry.resolved, "capsule://ato/postgres@sha256:abc123def");
    assert_eq!(entry.contract, "service@1");

    // §7.3.1 invariant 3: credentials in template form only.
    assert_eq!(
        entry.credentials.get("password").map(String::as_str),
        Some("{{env.PG_PASSWORD}}")
    );

    // §7.3 / §9.5: parameters are resolved values, identity-bearing.
    let database_value = match entry.parameters.get("database") {
        Some(crate::foundation::types::ParamValue::String(s)) => s.clone(),
        other => panic!("unexpected param value: {other:?}"),
    };
    assert_eq!(database_value, "appdb");

    // §9.5: identity_exports rendered from {{params.X}}.
    assert_eq!(
        entry.identity_exports.get("database").map(String::as_str),
        Some("appdb")
    );
    assert_eq!(
        entry.identity_exports.get("protocol").map(String::as_str),
        Some("postgresql")
    );
    assert_eq!(
        entry.identity_exports.get("major").map(String::as_str),
        Some("16")
    );

    // §7.7: instance_hash is blake3-128 prefix.
    assert!(entry.instance_hash.starts_with("blake3:"));
    assert_eq!(entry.instance_hash.len(), "blake3:".len() + 32);

    let state = entry.state.as_ref().expect("state present");
    assert_eq!(state.name, "data");
    assert_eq!(state.ownership, "parent");
    assert_eq!(state.version, "16");
}

#[test]
fn instance_hash_changes_on_parameter_change() {
    let consumer_a = parse(HAPPY_CONSUMER);
    let consumer_b_text = HAPPY_CONSUMER.replace("\"appdb\"", "\"otherdb\"");
    let consumer_b = parse(&consumer_b_text);

    let lock_a = verify_and_lock(happy_input(&consumer_a)).expect("verify a");
    let lock_b = verify_and_lock(happy_input(&consumer_b)).expect("verify b");
    assert_ne!(
        lock_a.entries["db"].instance_hash,
        lock_b.entries["db"].instance_hash
    );
}

#[test]
fn instance_hash_stable_across_credential_rotation() {
    // RFC §7.3.1 hard invariant 1: state path / instance_hash do not move when
    // credentials change. We model rotation by changing the env *reference* on
    // the consumer side; the lock template stays the same shape.
    let consumer_a = parse(HAPPY_CONSUMER);
    let consumer_b_text = HAPPY_CONSUMER
        .replace("{{env.PG_PASSWORD}}", "{{env.PG_PASSWORD_V2}}")
        .replace("[\"PG_PASSWORD\"]", "[\"PG_PASSWORD_V2\"]");
    let consumer_b = parse(&consumer_b_text);

    let lock_a = verify_and_lock(happy_input(&consumer_a)).expect("verify a");
    let lock_b = verify_and_lock(happy_input(&consumer_b)).expect("verify b");
    assert_eq!(
        lock_a.entries["db"].instance_hash, lock_b.entries["db"].instance_hash,
        "instance_hash must not move across credential rotation",
    );
}

#[test]
fn rule_3_contract_not_found() {
    let consumer = parse(&HAPPY_CONSUMER.replace("service@1", "service@2"));
    let err = verify_and_lock(happy_input(&consumer)).unwrap_err();
    assert!(
        matches!(err, LockError::ContractNotFound { .. }),
        "got {err:?}"
    );
}

#[test]
fn rule_4_target_not_found() {
    let bad_provider_text = HAPPY_PROVIDER.replace("target = \"server\"", "target = \"missing\"");
    let consumer = parse(HAPPY_CONSUMER);
    let mut providers = BTreeMap::new();
    providers.insert("db".to_string(), provider(&bad_provider_text));
    let err = verify_and_lock(DependencyLockInput {
        consumer: &consumer,
        providers,
    })
    .unwrap_err();
    assert!(
        matches!(err, LockError::TargetNotFound { .. }),
        "got {err:?}"
    );
}

#[test]
fn rule_5_parameter_required_but_missing() {
    let consumer = parse(&HAPPY_CONSUMER.replace("database = \"appdb\"\n", ""));
    let err = verify_and_lock(happy_input(&consumer)).unwrap_err();
    assert!(
        matches!(err, LockError::ParameterRequired { .. }),
        "got {err:?}"
    );
}

#[test]
fn rule_5_parameter_unknown_key() {
    let consumer = parse(&HAPPY_CONSUMER.replace(
        "database = \"appdb\"",
        "database = \"appdb\"\nmystery = \"x\"",
    ));
    let err = verify_and_lock(happy_input(&consumer)).unwrap_err();
    assert!(
        matches!(err, LockError::ParameterUnknown { .. }),
        "got {err:?}"
    );
}

#[test]
fn rule_6_credential_required_but_missing() {
    let consumer = parse(&HAPPY_CONSUMER.replace("password = \"{{env.PG_PASSWORD}}\"\n", ""));
    let err = verify_and_lock(happy_input(&consumer)).unwrap_err();
    assert!(
        matches!(err, LockError::CredentialRequired { .. }),
        "got {err:?}"
    );
}

#[test]
fn rule_6_credential_literal_forbidden() {
    let consumer =
        parse(&HAPPY_CONSUMER.replace("\"{{env.PG_PASSWORD}}\"", "\"plaintext-secret\""));
    let err = verify_and_lock(happy_input(&consumer)).unwrap_err();
    assert!(
        matches!(err, LockError::CredentialLiteralForbidden { .. }),
        "got {err:?}"
    );
}

#[test]
fn rule_6_credential_env_out_of_scope() {
    let consumer = parse(&HAPPY_CONSUMER.replace("[\"PG_PASSWORD\"]", "[]"));
    let err = verify_and_lock(happy_input(&consumer)).unwrap_err();
    assert!(
        matches!(err, LockError::CredentialEnvKeyOutOfScope { .. }),
        "got {err:?}"
    );
}

#[test]
fn rule_7_identity_export_contains_credential() {
    // Inject {{credentials.password}} into the identity_exports block.
    let bad_provider_text = HAPPY_PROVIDER.replace(
        "database = \"{{params.database}}\"",
        "database = \"{{credentials.password}}\"",
    );
    let consumer = parse(HAPPY_CONSUMER);
    let mut providers = BTreeMap::new();
    providers.insert("db".to_string(), provider(&bad_provider_text));
    let err = verify_and_lock(DependencyLockInput {
        consumer: &consumer,
        providers,
    })
    .unwrap_err();
    assert!(
        matches!(err, LockError::IdentityExportContainsCredential { .. }),
        "got {err:?}"
    );
}

#[test]
fn rule_8_state_required_but_missing() {
    // Strip [dependencies.db.state] block.
    let stripped = HAPPY_CONSUMER.replace("[dependencies.db.state]\nname = \"data\"\n", "");
    let consumer = parse(&stripped);
    let err = verify_and_lock(happy_input(&consumer)).unwrap_err();
    assert!(
        matches!(err, LockError::StateRequiredButMissing { .. }),
        "got {err:?}"
    );
}

#[test]
fn rule_8_state_version_missing() {
    let bad_provider_text = HAPPY_PROVIDER.replace("version = \"16\"\n", "");
    let consumer = parse(HAPPY_CONSUMER);
    let mut providers = BTreeMap::new();
    providers.insert("db".to_string(), provider(&bad_provider_text));
    let err = verify_and_lock(DependencyLockInput {
        consumer: &consumer,
        providers,
    })
    .unwrap_err();
    assert!(
        matches!(err, LockError::StateVersionMissing { .. }),
        "got {err:?}"
    );
}

#[test]
fn rule_9_needs_not_in_dependencies() {
    let consumer = parse(&HAPPY_CONSUMER.replace("needs = [\"db\"]", "needs = [\"missing\"]"));
    let err = verify_and_lock(happy_input(&consumer)).unwrap_err();
    assert!(
        matches!(err, LockError::NeedsNotInDependencies { .. }),
        "got {err:?}"
    );
}

#[test]
fn rule_11_major_version_conflict() {
    // Add a second dep that points at the same source path with a different
    // major. The lock-time grouper detects this even before per-dep checks.
    let consumer_text = format!(
        r#"{}

[dependencies.db_old]
capsule = "capsule://ato/postgres@15"
contract = "service@1"

[dependencies.db_old.parameters]
database = "old"

[dependencies.db_old.credentials]
password = "{{{{env.PG_PASSWORD}}}}"

[dependencies.db_old.state]
name = "old"
"#,
        HAPPY_CONSUMER
    );
    let consumer = parse(&consumer_text);
    let mut providers = BTreeMap::new();
    providers.insert("db".to_string(), provider(HAPPY_PROVIDER));
    providers.insert(
        "db_old".to_string(),
        ResolvedProviderManifest {
            requested: "capsule://ato/postgres@15".to_string(),
            resolved: "capsule://ato/postgres@sha256:old".to_string(),
            manifest: parse(HAPPY_PROVIDER),
        },
    );
    let err = verify_and_lock(DependencyLockInput {
        consumer: &consumer,
        providers,
    })
    .unwrap_err();
    assert!(
        matches!(err, LockError::MajorVersionConflict { .. }),
        "got {err:?}"
    );
}

#[test]
fn rule_12_instance_uniqueness_violation() {
    // Two aliases with identical (resolved, contract, parameters) →
    // identical instance_hash → fail.
    let consumer_text = format!(
        r#"{}

[dependencies.db2]
capsule = "capsule://ato/postgres@16"
contract = "service@1"

[dependencies.db2.parameters]
database = "appdb"

[dependencies.db2.credentials]
password = "{{{{env.PG_PASSWORD}}}}"

[dependencies.db2.state]
name = "data2"
"#,
        HAPPY_CONSUMER
    );
    let consumer = parse(&consumer_text);
    let mut providers = BTreeMap::new();
    providers.insert("db".to_string(), provider(HAPPY_PROVIDER));
    providers.insert("db2".to_string(), provider(HAPPY_PROVIDER));
    let err = verify_and_lock(DependencyLockInput {
        consumer: &consumer,
        providers,
    })
    .unwrap_err();
    assert!(
        matches!(err, LockError::InstanceUniquenessViolation { .. }),
        "got {err:?}"
    );
}

#[test]
fn rule_13_reserved_ready_http_fails_closed() {
    let bad_provider_text = HAPPY_PROVIDER.replace(
        "ready = { type = \"probe\", run = \"pg_isready\", timeout = \"30s\" }",
        "ready = { type = \"http\", url = \"http://x\", timeout = \"30s\" }",
    );
    let consumer = parse(HAPPY_CONSUMER);
    let mut providers = BTreeMap::new();
    providers.insert("db".to_string(), provider(&bad_provider_text));
    let err = verify_and_lock(DependencyLockInput {
        consumer: &consumer,
        providers,
    })
    .unwrap_err();
    assert!(
        matches!(
            err,
            LockError::ReservedVariantReadyProbe { ref variant, .. } if variant == "http"
        ),
        "got {err:?}"
    );
}

#[test]
fn rule_13_reserved_ready_unix_socket_fails_closed() {
    let bad_provider_text = HAPPY_PROVIDER.replace(
        "ready = { type = \"probe\", run = \"pg_isready\", timeout = \"30s\" }",
        "ready = { type = \"unix_socket\", path = \"/tmp/p.sock\", timeout = \"30s\" }",
    );
    let consumer = parse(HAPPY_CONSUMER);
    let mut providers = BTreeMap::new();
    providers.insert("db".to_string(), provider(&bad_provider_text));
    let err = verify_and_lock(DependencyLockInput {
        consumer: &consumer,
        providers,
    })
    .unwrap_err();
    assert!(
        matches!(
            err,
            LockError::ReservedVariantReadyProbe { ref variant, .. } if variant == "unix_socket"
        ),
        "got {err:?}"
    );
}

#[test]
fn parameter_default_filled_when_consumer_omits() {
    let provider_text = HAPPY_PROVIDER.replace(
        "database = { type = \"string\", required = true }",
        "database = { type = \"string\", required = false, default = \"defaultdb\" }",
    );
    let consumer_text = HAPPY_CONSUMER.replace("database = \"appdb\"\n", "");
    let consumer = parse(&consumer_text);
    let mut providers = BTreeMap::new();
    providers.insert("db".to_string(), provider(&provider_text));
    let lock = verify_and_lock(DependencyLockInput {
        consumer: &consumer,
        providers,
    })
    .expect("verify with default");
    let database_value = match lock.entries["db"].parameters.get("database") {
        Some(crate::foundation::types::ParamValue::String(s)) => s.clone(),
        other => panic!("unexpected: {other:?}"),
    };
    assert_eq!(database_value, "defaultdb");
}
