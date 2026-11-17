//! Reproduction of the pure decision logic behind the hosted Runner / Formation
//! attempt lease findings.  Every function below is copied from the real source
//! (file and line named above it) with the surrounding I/O removed; nothing is
//! linked against the crates, so rustc alone can run it:
//!
//!   rustc -O -o .tmp/hosted_lease_logic formal/lean-bugcheck/repro/hosted_lease_logic.rs
//!   ./.tmp/hosted_lease_logic
//!
//! Findings exercised here: H2 (max_duration_secs is not an absolute cap and
//! expires_at does not bound the Run), H1/H4 (the lease failure code chain) and
//! H1-journal (the attempt barrier is per work root).

// ── apps/connected-realization-worker/src/runtime_launch/lease.rs:60-63, 800-809
const RUNTIME_LAUNCH_MAX_DURATION_SECS: u64 = 60 * 60;

#[derive(Default, Clone, Copy)]
#[allow(dead_code)]
struct ExecutionAuthorization {
    policy_generation: u64,
    authorization_generation: u64,
}

#[derive(Default)]
struct LeaseCommand {
    max_duration_secs: Option<u64>,
    execution_authorization: Option<ExecutionAuthorization>,
}

/// lease.rs:800-809, verbatim.
fn maximum_lifetime(command: &LeaseCommand) -> Option<u64> {
    match (
        command.max_duration_secs,
        command.execution_authorization.as_ref(),
    ) {
        (Some(seconds), _) => Some(seconds),
        (None, Some(_)) => None,
        (None, None) => Some(RUNTIME_LAUNCH_MAX_DURATION_SECS),
    }
}

// ── apps/connected-realization-worker/src/lib.rs:3951-3956 (validate_lease)
/// The only place the lease's own expires_at is consulted.  remaining is
/// expires_at - now in seconds; the real code refuses when it is <= 0.
fn lease_admitted(expires_at_remaining_secs: i64) -> bool {
    expires_at_remaining_secs > 0
}

// ── apps/connected-realization-worker/src/lib.rs:2213-2221
/// The wire code the Runner reports for a failed lease, verbatim shape.
fn failure_code(message: &str, insufficient_storage: bool) -> &'static str {
    if insufficient_storage {
        "storage_insufficient_space"
    } else if message.contains("execution authorization") {
        "execution_authorization_ended"
    } else {
        "realization_failed"
    }
}

// ── lib/runtime-attempt/src/journal.rs:232-275 (AttemptJournal::reserve)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AttemptState {
    NotStarted,
    Started,
    Finished,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Record {
    attempt_id: u64,
    state: AttemptState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refusal {
    AlreadyStarted(AttemptState),
    Unknown(u64),
}

type Journal = Vec<Record>;

/// journal.rs:245-275 with the per-request directory replaced by the record list.
fn reserve(journal: &Journal, attempt_id: u64) -> Result<Journal, Refusal> {
    // "Exact history takes precedence over any sibling, independent of directory order."
    if let Some(existing) = journal.iter().find(|r| r.attempt_id == attempt_id) {
        return Err(Refusal::AlreadyStarted(existing.state));
    }
    if let Some(existing) = journal
        .iter()
        .find(|r| r.state == AttemptState::Started)
    {
        return Err(Refusal::Unknown(existing.attempt_id));
    }
    let mut next = journal.clone();
    next.push(Record {
        attempt_id,
        state: AttemptState::NotStarted,
    });
    Ok(next)
}

fn main() {
    // ── H2: max_duration_secs, and the documented one-hour cap ───────────────
    let no_cap = LeaseCommand {
        max_duration_secs: None,
        execution_authorization: None,
    };
    assert_eq!(maximum_lifetime(&no_cap), Some(3600));

    let with_authorization = LeaseCommand {
        max_duration_secs: None,
        execution_authorization: Some(ExecutionAuthorization {
            policy_generation: 1,
            authorization_generation: 1,
        }),
    };
    // RUNTIME_LAUNCH_MAX_DURATION_SECS' doc: "omitting the field preserves the
    // ordinary one-hour safety cap."  It does not when an execution
    // authorization is attached: there is no local cap at all.
    assert_eq!(maximum_lifetime(&with_authorization), None);
    println!(
        "H2  omitted max_duration_secs -> no cap: {:?} (doc promises Some(3600))",
        maximum_lifetime(&with_authorization)
    );

    let shorter = LeaseCommand {
        max_duration_secs: Some(60),
        execution_authorization: None,
    };
    assert_eq!(maximum_lifetime(&shorter), Some(60));
    println!(
        "    explicit shorter cap is honoured: {:?}",
        maximum_lifetime(&shorter)
    );

    // ── H2b: expires_at bounds admission only ───────────────────────────────
    let allowed = lease_admitted(1);
    let run_seconds = maximum_lifetime(&no_cap).unwrap();
    assert!(allowed && run_seconds > 1);
    println!(
        "H2b lease accepted with 1s of expires_at left: admitted={allowed}, Run may last {run_seconds}s"
    );

    // ── H1/H4: the lease failure code chain ─────────────────────────────────
    let malformed = "lease command launch_spec is invalid: unknown protocol";
    let digest = "launch spec digest mismatch: lease says sha256:aa, spec digests to sha256:bb";
    let admission = "execution authorization expired";
    let expiry = "the Run exceeded its maximum lifetime without a stop request";
    let commit = "the Run's workload could not be confirmed stopped; its state was quarantined";
    for (label, message) in [
        ("format invalid (launch_spec)", malformed),
        ("format invalid (digest)", digest),
        ("authorization ended", admission),
        ("normal max-lifetime expiry", expiry),
        ("state commit failed", commit),
    ] {
        println!("H1  {label:32} -> {}", failure_code(message, false));
    }
    assert_eq!(failure_code(malformed, false), "realization_failed");
    assert_eq!(failure_code(digest, false), "realization_failed");
    assert_eq!(failure_code(expiry, false), "realization_failed");
    assert_eq!(failure_code(commit, false), "realization_failed");
    assert_ne!(
        failure_code(malformed, false),
        failure_code(admission, false)
    );

    // The separator is a substring match on prose, so any message that happens
    // to contain the words - including one whose cause is something else -
    // becomes execution_authorization_ended.
    let unrelated = "cannot persist the execution authorization renewal journal";
    assert_eq!(
        failure_code(unrelated, false),
        "execution_authorization_ended"
    );
    println!("H1  prose match: {unrelated:?} -> execution_authorization_ended");
    let reworded = "the Coordinator stopped authorizing this Run";
    assert_eq!(failure_code(reworded, false), "realization_failed");
    println!("H1  same cause, reworded message -> realization_failed (no test pins the wording)");

    // ── H1-journal: the attempt barrier's scope ─────────────────────────────
    // Work root A ran attempt 7 and died before recording its end.
    let work_root_a: Journal = vec![Record {
        attempt_id: 7,
        state: AttemptState::Started,
    }];
    assert_eq!(reserve(&work_root_a, 8), Err(Refusal::Unknown(7)));
    assert_eq!(
        reserve(&work_root_a, 7),
        Err(Refusal::AlreadyStarted(AttemptState::Started))
    );
    println!("H1j same work root: UNKNOWN attempt 7 blocks 7 and 8");

    // The same job is redelivered to a Runner whose work root has no history.
    let work_root_b: Journal = Vec::new();
    let admitted = reserve(&work_root_b, 7).expect("fresh work root admits the same id");
    assert_eq!(admitted[0].attempt_id, 7);
    println!("H1j different work root: attempt 7 is admitted again (barrier is local)");

    // A finished attempt lets a new attempt of the same request start, which is
    // intended; the point is that only the local journal can tell them apart.
    let finished: Journal = vec![Record {
        attempt_id: 7,
        state: AttemptState::Finished,
    }];
    assert!(reserve(&finished, 8).is_ok());
    assert!(reserve(&finished, 7).is_err());

    println!("all hosted lease decision-logic assertions held");
}
