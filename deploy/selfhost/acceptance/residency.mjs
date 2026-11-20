// Is this Instance resident, i.e. will it still be running tomorrow?
//
// "Run is ready and its Contract is verified" does not answer that. A Run
// started without a renewable execution authorization is ended by the Runner
// at its lifetime cap (one hour) and nothing starts it again. Resident means:
// the owner's policy is always_on / running and the Coordinator has reconciled
// it, the policy's current Run holds an active, unexpired authorization issued
// for this policy generation, and that Run's lease is ready with no stop
// requested.
//
// Reads the Coordinator's SQLite database without writing. residency.sh runs
// this inside the api container; it can also be imported.
import { DatabaseSync } from "node:sqlite";
import { join } from "node:path";

export function residency(db, instanceId, now = new Date()) {
  const policy = db
    .prepare(
      `SELECT mode, desired_state, reconcile_status, generation, last_run_id,
              last_failure_code
         FROM instance_runtime_policies WHERE compute_instance_id = ?`,
    )
    .get(instanceId);
  const reasons = [];
  if (!policy) {
    return {
      instance_id: instanceId,
      resident: false,
      reasons: ["no runtime policy: the Instance runs on demand and its Run ends at the lifetime cap"],
    };
  }
  if (policy.mode !== "always_on" || policy.desired_state !== "running")
    reasons.push(`policy is ${policy.mode}/${policy.desired_state}, not always_on/running`);
  if (policy.reconcile_status !== "running")
    reasons.push(
      `policy reconcile_status is ${policy.reconcile_status}` +
        (policy.last_failure_code ? ` (last failure: ${policy.last_failure_code})` : ""),
    );
  const grant = policy.last_run_id
    ? db
        .prepare(
          `SELECT a.status, a.expires_at, a.renewed_at, a.policy_generation,
                  a.authorization_generation, l.status AS lease_status,
                  l.stop_requested_at
             FROM run_execution_authorizations a
             JOIN runner_leases l ON l.id = a.lease_id
            WHERE a.run_id = ? AND a.compute_instance_id = ?`,
        )
        .get(policy.last_run_id, instanceId)
    : undefined;
  if (!grant) {
    reasons.push("the policy's Run has no execution authorization: it ends at the lifetime cap");
  } else {
    if (grant.status !== "active") reasons.push(`execution authorization is ${grant.status}`);
    if (Date.parse(grant.expires_at) <= now.getTime())
      reasons.push(`execution authorization expired at ${grant.expires_at}`);
    if (grant.policy_generation !== policy.generation)
      reasons.push("execution authorization belongs to an earlier policy generation");
    if (grant.lease_status !== "ready") reasons.push(`lease is ${grant.lease_status}, not ready`);
    if (grant.stop_requested_at) reasons.push("a stop was requested for the lease");
  }
  return {
    instance_id: instanceId,
    resident: reasons.length === 0,
    reasons,
    run_id: policy.last_run_id ?? null,
    policy: {
      mode: policy.mode,
      desired_state: policy.desired_state,
      reconcile_status: policy.reconcile_status,
      generation: policy.generation,
    },
    authorization: grant
      ? {
          status: grant.status,
          expires_at: grant.expires_at,
          renewed_at: grant.renewed_at,
          authorization_generation: grant.authorization_generation,
        }
      : null,
  };
}

if (process.argv[1] === "-") {
  const instanceId = process.argv[2];
  if (!instanceId || !process.env.ATO_DATA_DIR) {
    console.error("usage: residency.sh INSTANCE_ID");
    process.exit(2);
  }
  const db = new DatabaseSync(join(process.env.ATO_DATA_DIR, "coordinator.sqlite"), {
    readOnly: true,
  });
  const result = residency(db, instanceId);
  console.log(JSON.stringify(result, null, 2));
  process.exit(result.resident ? 0 : 1);
}
