# Scenario verifier v1 (CLI local, Linux)

Status: operating note / acceptance record
Date: 2026-10-25
Scope: `ato run` and `ato reform` on a **Linux CLI host** with a process
candidate. Formation, hosted validation and Runtime Network do **not** execute
scenarios in v1 and fail closed when K requires one.

## What a Scenario is

A Scenario is one more Contract requirement, not a new semantic root:

```toml
[[contract.observation]]
id = "crud"
use = "ato.contract.scenario@1"
port = "app.http"
scenario = "scenarios/crud.json"
```

`ato pack` reads the source-relative file, canonicalises it into an
`ato.scenario/1` object, and the requirement carries only the object's
`sha256:` reference in the existing `digest` field. `BoundContract` keeps its
`{schema, requirements}` shape, so `K` bytes for Contracts without a scenario
do not move. The steps live in the object; the implementation and runtime stay
in the Derivation.

The Scenario object (canonical JCS) states:

- `verifier_version: 1` (the only version this build executes);
- `isolation: { fresh_instance: true, network: "candidate-only" }` (required);
- bounded `limits` (step/total timeout, max body size);
- ordered `steps`: method + origin-relative path + optional headers/body,
  expected status/body digest/JSON assertions, and optional JSON-pointer
  captures substituted into later steps as `${name}`;
- a step may set `cookies: "cleared"` to send that request with no session
  (the negative case), while later steps keep the cookie jar.

## What the runtime does

`ato run` starts a fresh candidate, performs the Contract's existing read-only
GET observations, and then executes each Scenario against the candidate's own
endpoints:

- redirects are not followed and paths are origin-relative, so the verifier
  only addresses candidate endpoints (this is not a claim that the application
  cannot reach the network);
- cookies are a manual `Set-Cookie: name=value` jar (`cleared` applies to that
  request only);
- a captured value is normalised to `${name}` in the hashed transcript, so an
  app that returns a fresh id still produces a stable digest;
- the raw transcript (resolved paths/bodies, bounded response bodies,
  captures, failures) is written next to the Run as
  `scenario-transcripts/<requirement-id>.json`, and kept beside the receipt as
  `<receipt>.scenarios/<requirement-id>.json` for `ato run`, or under
  `<out>/scenario-transcripts/` for `ato reform`;
- **v1 only executes on a fresh instance.** A candidate whose Derivation
  declares a state slot, or which restores a portable Instance snapshot, is
  refused with `scenario_isolation_unavailable` instead of executing a
  scenario against restored state.

## K, transcript and receipt mapping

The receipt schema is unchanged (`ato.contract-verification-receipt/1`), and
nothing is added to `K`:

| receipt evidence | scenario value |
|---|---|
| `path` | `scenario:<requirement id>` |
| `body_sha256` | digest of the canonical transcript (`ato.scenario-transcript/1`, JCS) |
| `digest` | the `ato.scenario/1` object reference K pins |
| `method` / `status` / `input` | absent |

`fully_satisfied` keeps its existing meaning: the defined Contract `K` was
satisfied. It is not a completeness, security or "cannot reach the network"
claim.

## Verification matrix

| condition | outcome |
|---|---|
| no observation for a scenario requirement (Formation, hosted) | `scenario_observation_missing` |
| executed object ref ≠ K's digest | `scenario_object_mismatch` |
| unknown `verifier_version` | `scenario_verifier_version_unsupported` |
| transcript digest is not a sha256 reference | `scenario_transcript_mismatch` |
| a step misses its expectation | `scenario_step_failed` (detail names the step) |
| state slot / restored snapshot | `scenario_isolation_unavailable` |
| every step satisfied and the digest is well formed | `Satisfied` |

Seal-time `verify()` (Formation, before the candidate can run) returns
`Deferred { by: "runtime scenario verifier <id>" }`: a Formation receipt can be
seal-admissible but is never fully satisfied by a deferred scenario.

## Reproduction (Linux)

```sh
cargo test -p ato-cli --test scenario_verification -- --nocapture
```

The suite packs `apps/cli/tests/fixtures/scenario-v1/{good,bad,isolation}` and
runs `ato pack` → `ato run --no-open --verification-receipt`, then `ato reform`
with the `candidate` source under the frozen K. Evidence is written to
`target/scenario-verifier-v1-evidence/evidence.json` (schema
`ato.scenario-verifier-evidence/1`) with the receipt and transcript copies; CI
uploads that directory from the Linux job.

The fixture app is a Python stdlib server with an in-memory item list and a
session cookie: login → create (captures the id) → update (body digest pinned)
→ unauthorized read with cleared cookies → 401 → delete → read-after-delete →
404. The `bad` source answers the unauthorized read like an authorized one;
the `isolation` manifest declares a state slot; the `candidate` source is a
different implementation of the same behavior.

## Explicit non-claims

- Completeness is not claimed: `fully_satisfied` means the defined `K` was
  satisfied by this Run, nothing more.
- The verifier does not prove that the application cannot reach the network.
  Local process candidates share the host network; the verifier proves only
  that **it** sent requests to candidate endpoints with origin-relative paths.
- No external-side-effect proof exists beyond the steps a Contract declares.
- Formation local/hosted, hosted validation, Runtime Network and retained
  replay do not execute scenarios in v1: a scenario requirement there fails
  closed as unobserved. Extending the API relay/validator is B3.
- Secrets must not be written into Scenarios; captures and response bodies are
  evidence and are stored in the raw sidecar.
- Scenario execution starts no browser, publishes nothing and activates
  nothing.
