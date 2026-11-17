//! task-8 repro: a durable local Instance whose Run lease is left in the
//! 'starting' status has no user-facing recovery path.
//!
//! Evidence for the Lean model InstanceLease.starting_lease_closed_to_user_commands
//! and InstanceLease.starting_lease_is_absorbing.
//!
//! Run from an isolated directory: OUT=<dir> instance_lease.
//! Writes only below $OUT.

use std::path::PathBuf;

use ato_portable_application::local_instance::{LocalApplicationStore, LocalInstanceRunStatus};

const INSTANCE: &str = "linst_test";

/// Canonical (serde_jcs) LocalInstanceMetadata: keys sorted, no whitespace.
/// This is exactly what LocalApplicationStore::import would write; the repro
/// only needs instance() to accept it so that claim_run can be reached without
/// a real portable bundle.
fn canonical_instance_json() -> String {
    format!(
        concat!(
            "{{\"application_id\":\"lapp_test\",",
            "\"application_ref\":\"{app}\",",
            "\"available_derivation_refs\":[\"{d}\"],",
            "\"bindings\":{{}},",
            "\"bundle_sha256\":\"{bundle}\",",
            "\"contract_ref\":\"{k}\",",
            "\"created_at\":\"0\",",
            "\"data_snapshot_ref\":null,",
            "\"instance_id\":\"{inst}\",",
            "\"schema\":\"ato.local-instance/1\",",
            "\"selected_derivation_ref\":\"{d}\"}}"
        ),
        app = "sha256:".to_owned() + &"aa".repeat(32),
        d = "sha256:".to_owned() + &"dd".repeat(32),
        bundle = "sha256:".to_owned() + &"bb".repeat(32),
        k = "sha256:".to_owned() + &"cc".repeat(32),
        inst = INSTANCE,
    )
}

fn main() {
    let out = std::env::var("OUT").expect("set OUT to an isolated writable directory");
    let root = PathBuf::from(out).join("instance-lease");
    if root.exists() {
        std::fs::remove_dir_all(&root).expect("clear previous repro root");
    }
    let instance_root = root.join("instances").join(INSTANCE);
    std::fs::create_dir_all(&instance_root).expect("create instance dir");
    std::fs::write(
        instance_root.join("instance.json"),
        canonical_instance_json(),
    )
    .expect("write canonical instance metadata");

    let store = LocalApplicationStore::open(&root).expect("open store");
    store.instance(INSTANCE).expect("metadata is canonical and named");

    // 1. claim_run reaches 'starting' without a bundle. The claiming process is
    //    then gone (simulated by never calling activate_run).
    let run = store.claim_run(INSTANCE).expect("first claim");
    assert_eq!(run.status, LocalInstanceRunStatus::Starting);
    let token = run.token.clone();
    let on_disk = std::fs::read_to_string(instance_root.join("active-run.json"))
        .expect("read active-run.json");

    // 2. A second start is refused, whatever the status is.
    let conflict = store
        .claim_run(INSTANCE)
        .expect_err("second claim must fail");
    let conflict_text = conflict.to_string();
    assert!(
        conflict_text.contains("already has Run") && conflict_text.contains("starting"),
        "{conflict_text}"
    );

    // 3. Another controller cannot clear it: both operations fence on the token.
    let release_err = store
        .release_run(INSTANCE, "token_not_the_one")
        .expect_err("release with a foreign token must fail");
    let quarantine_err = store
        .quarantine_run(INSTANCE, "token_not_the_one", "repro", &[])
        .expect_err("quarantine with a foreign token must fail");

    // 4. The token IS on disk, so this is not a secrecy problem: it is the
    //    absence of a command that acts on a non-Active lease.
    let token_is_recorded = on_disk.contains(&token);

    // 5. Only the token holder can leave the state.
    store
        .release_run(INSTANCE, &token)
        .expect("token holder releases");
    let reclaimed = store.claim_run(INSTANCE).expect("claim after release");
    assert_eq!(reclaimed.status, LocalInstanceRunStatus::Starting);
    store.release_run(INSTANCE, &reclaimed.token).expect("cleanup");

    println!("instance lease repro: PASS");
    println!("  second claim            : {conflict_text}");
    println!("  release with bad token  : {release_err}");
    println!("  quarantine with bad tok : {quarantine_err}");
    println!("  token present on disk   : {token_is_recorded}");
    println!("  user commands on a starting lease: ato app start -> claim_run refused;");
    println!("  ato app stop -> \"still preparing and cannot be stopped\" (apps/cli/src/lib.rs:2135)");
}
