# Launch Preparation Bridge Contract (#581 ↔ #593)

Status: draft · Date: 2026-06-09 · SSOT: `crates/capsule-core/src/engine/launch_preparation_bridge.rs`

## Purpose

[#581](https://github.com/ato-run/ato/issues/581) composes a **launch preparation
plan** inside `capsule-core` (`engine::launch_preparation::prepare_launch`). The
[#593](https://github.com/ato-run/ato-api/issues) managed-runner control plane
(ato-api) cannot import Rust and does not need the full plan. This document
defines the **stable JSON boundary** between the two: `LaunchPreparationBridgeResult`.

It is a *bridge contract*, not a transport. capsule-core does not call ato-api,
and ato-api does not (yet) invoke `prepare_launch` directly — until a real core
service / worker / CLI integration exists, ato-api uses a dev/test provider that
emits this exact shape. See `ato-api/src/services/launch_preparation.ts`.

- Schema: [`launch-preparation-plan.schema.json`](./launch-preparation-plan.schema.json)
- Golden fixtures: `crates/capsule-core/tests/fixtures/launch_preparation/`
  - `prepared_managed_runner.json`
  - `not_prepared_standard_install.json`
- Contract tests: `crates/capsule-core/tests/launch_preparation_bridge_fixtures.rs`
  and the `launch_preparation_bridge_*` lib tests.

## Shape

A discriminated union tagged on `status`.

### `prepared`

```jsonc
{
  "status": "prepared",
  "plan": {
    "install_revision_id": "rev_<32 hex>",
    "capsule_instance_key": "cik_<32 hex>",
    "execution_id": "exec_<32+ hex>",
    "requirement_graph_hash": "blake3:<64 hex>",            // graph CONTENT hash
    "requirement_graph_snapshot_hash": "blake3:<64 hex>",   // snapshot identity — DISTINCT
    "launch_template_key_hash": "blake3:<64 hex>",
    "selected_runner_class": "managed_runner",              // snake_case RunnerClass
    "selected_runner_ref": "/runners/run_managed_1",
    "command_request_id": "cmdreq_1",
    "prepare_command": {                                    // PrepareSession ONLY
      "command": "prepare_session",
      "session": "ses_prep",
      "materialization_plan": "/sessions/cik_.../materialization"
    }
  }
}
```

### `not_prepared`

```jsonc
{
  "status": "not_prepared",
  "blockers": [
    { "code": "launch_template_not_reusable", "detail": "…optional, non-authoritative…" }
  ]
}
```

## Invariants (enforced by tests)

1. `requirement_graph_hash` ≠ `requirement_graph_snapshot_hash` (#588/#596) — never collapsed.
2. `prepare_command.command` is always `prepare_session` in a prepared plan — never
   `start_session` / `stop_session`.
3. `selected_runner_class` is `managed_runner` for the managed-cloud fixture.
4. The bridge **omits** the nested `launch_template` and `materialization` records
   that the in-process plan carries — only the flat refs above cross the boundary.
5. No raw secret value and no observed/runtime diagnostic
   (`observed_status`, `readiness_status`, `dynamic_port`, `process_id`,
   `container_id`, `log_cursor`, `live_route`) ever appears — not even in `detail`.
6. `execution_id` is deterministic from stable inputs; it is **not** an observed
   pid/container id. `materialized_at` (metadata) is not part of plan identity.

## Blocker vocabulary

`code` is a closed set. capsule-core maps its typed
`LaunchPreparationBlocker` variants to these codes
(`engine::launch_preparation_bridge::bridge_blocker_code`):

| capsule-core blocker variant        | bridge `code`                    |
| ----------------------------------- | -------------------------------- |
| `ReusableInputsInvalid`             | `reusable_inputs_invalid`        |
| `LaunchTemplateNotReusable`         | `launch_template_not_reusable`   |
| `LaunchMaterializationFailed`       | `launch_materialization_failed`  |
| `MaterializationPersistFailed`      | `launch_materialization_failed`  |
| `PrepareSessionCommandFailed`       | `prepare_session_command_failed` |
| _(caller cannot reach capsule-core)_| `launch_preparation_unavailable` |

`launch_preparation_unavailable` is reserved for the *consumer* side: a control
plane whose launch-preparation provider is disabled or unreachable emits it
without capsule-core having run at all.

### How ato-api maps bridge codes → Run blockers (#593)

The control plane maps conservatively onto its own `RunBlocker` union:

| bridge `code`                    | ato-api Run blocker             |
| -------------------------------- | ------------------------------- |
| `launch_template_not_reusable`   | `launch_preparation_not_ready`  |
| `reusable_inputs_invalid`        | `launch_preparation_not_ready`  |
| `launch_materialization_failed`  | `launch_preparation_failed`     |
| `prepare_session_command_failed` | `launch_preparation_failed`     |
| `launch_preparation_unavailable` | `launch_preparation_unavailable`|

## Stability

The plan is internal to #581 today (Serialize/Deserialize only). Future changes:

- Never collapse the two requirement-graph hashes into one field.
- Keep `prepare_command` a tagged object; never flatten to an untyped blob.
- Add new optional fields with defaults; never add required fields.
- Regenerate the golden fixtures with
  `cargo test -p capsule-core --lib regenerate_launch_preparation_bridge_golden_fixtures -- --ignored`
  and review the diff when the shape changes intentionally.
