import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import test from "node:test";
import { residency } from "./residency.mjs";

const now = new Date("2026-11-20T12:00:00.000Z");

function world({ policy, grant, lease } = {}) {
  const db = new DatabaseSync(":memory:");
  db.exec(`
    CREATE TABLE instance_runtime_policies (compute_instance_id TEXT PRIMARY KEY, mode TEXT,
      desired_state TEXT, reconcile_status TEXT, generation INTEGER, last_run_id TEXT,
      last_failure_code TEXT);
    CREATE TABLE run_execution_authorizations (run_id TEXT PRIMARY KEY, lease_id TEXT,
      compute_instance_id TEXT, policy_generation INTEGER, authorization_generation INTEGER,
      status TEXT, expires_at TEXT, renewed_at TEXT);
    CREATE TABLE runner_leases (id TEXT PRIMARY KEY, run_id TEXT, status TEXT,
      stop_requested_at TEXT);`);
  if (policy !== null) {
    const p = { mode: "always_on", desired_state: "running", reconcile_status: "running",
      generation: 2, last_run_id: "run_1", last_failure_code: null, ...policy };
    db.prepare("INSERT INTO instance_runtime_policies VALUES ('ci_1', ?, ?, ?, ?, ?, ?)")
      .run(p.mode, p.desired_state, p.reconcile_status, p.generation, p.last_run_id, p.last_failure_code);
  }
  if (grant !== null) {
    const g = { policy_generation: 2, authorization_generation: 3, status: "active",
      expires_at: "2026-11-20T12:10:00.000Z", ...grant };
    db.prepare("INSERT INTO run_execution_authorizations VALUES ('run_1', 'lease_1', 'ci_1', ?, ?, ?, ?, ?)")
      .run(g.policy_generation, g.authorization_generation, g.status, g.expires_at, "2026-11-20T11:55:00.000Z");
  }
  const l = { status: "ready", stop_requested_at: null, ...lease };
  db.prepare("INSERT INTO runner_leases VALUES ('lease_1', 'run_1', ?, ?)").run(l.status, l.stop_requested_at);
  return db;
}

test("a renewed always-on Run is resident", () => {
  const result = residency(world(), "ci_1", now);
  assert.equal(result.resident, true);
  assert.deepEqual(result.reasons, []);
  assert.equal(result.authorization.authorization_generation, 3);
});

test("a first run is ready and verified but not resident", () => {
  // import-app {start:true}: no policy row, no authorization, a ready lease.
  const result = residency(world({ policy: null, grant: null }), "ci_1", now);
  assert.equal(result.resident, false);
  assert.match(result.reasons[0], /lifetime cap/);
});

for (const [name, input, reason] of [
  ["on-demand policy", { policy: { mode: "on_demand", desired_state: "stopped", reconcile_status: "idle" }, grant: null }, /not always_on/],
  ["policy not reconciled", { policy: { reconcile_status: "degraded", last_failure_code: "realization_failed" } }, /degraded \(last failure: realization_failed\)/],
  ["no authorization", { grant: null }, /no execution authorization/],
  ["revoked authorization", { grant: { status: "revoked" } }, /is revoked/],
  ["expired authorization", { grant: { expires_at: "2026-11-20T11:59:59.000Z" } }, /expired at/],
  ["authorization of an older policy", { grant: { policy_generation: 1 } }, /earlier policy generation/],
  ["lease not ready", { lease: { status: "failed" } }, /lease is failed/],
  ["stop requested", { lease: { stop_requested_at: "2026-11-20T11:59:00.000Z" } }, /stop was requested/],
]) {
  test(`not resident: ${name}`, () => {
    const result = residency(world(input), "ci_1", now);
    assert.equal(result.resident, false);
    assert.ok(result.reasons.some((item) => reason.test(item)), result.reasons.join("; "));
  });
}
