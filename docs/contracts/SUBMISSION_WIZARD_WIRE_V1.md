# ato.submission-wizard-wire/v1

Submission Wizard — PR-0 Wire Contract (types only, nothing wired).

> **This file is the single source of truth (SSOT) for the submission-wizard
> wire contract.** It lives in the ato repo at
> `docs/contracts/SUBMISSION_WIZARD_WIRE_V1.md` and is versioned with the code
> that implements it. Any copy in a scratchpad, session directory, or other
> repo is **non-normative**; on divergence, this file wins.

Status: SPEC for PR-0, revised per contract review (blockers B1–B3, decisions
D1–D3 — see CHANGELOG §11). Defines every wire message, enum, ID format, and
the capsule.toml declaration schema for the interactive-capture submission
wizard. PR-0 lands **types + validation + tests only** on both sides:

- ato-api: zod schemas + constants in `src/services/submission_wizard/wire.ts`
  (new service module; routes keep wire schemas per existing convention, but in
  PR-0 nothing is mounted — schemas live in the service so PR-1 routes can
  import them, mirroring `session_surface.ts` → `capsule_snapshots.ts`).
- ato (Rust): serde types + TOML validation in
  `tools/snapshot-builder/src/wizard_wire.rs`, declared via `mod wizard_wire;`
  in `main.rs` (the `upload.rs` precedent), with `#[cfg(test)] mod tests`
  inside. No lib target; the crate already depends on serde/serde_json, and
  PR-0 adds `toml` to it (the workspace already pins toml 0.8, so this is a
  `Cargo.toml` one-liner + an existing lock entry).

## Wire contract version

Both sides define the constant:

```text
wire_contract_version = "ato.submission-wizard-wire/v1"
```

The claim response extension (§3.1) carries it as a **required literal
field** (`wire_contract_version`). A value other than the exact literal is a
schema rejection on both sides — version mismatch **fails closed** at parse
time, before any semantics run.

Grounding conventions (from the live codebase, do not deviate):

- Wire JSON is **snake_case**. TS/drizzle identifiers stay camelCase.
- **String length bounds are measured in UTF-16 code units** — what the live
  api's zod `.min()/.max()` count (`String.length`). The Rust mirror uses
  `s.encode_utf16().count()`, never `chars().count()`, so a string gets one
  verdict on both sides; an astral scalar counts as 2. This is behavioral for
  the charset-unrestricted bounds (hold-ready `builder_id`/`slot_id`/
  `session_id` ≤ 120, terminal-ack `agent_id` ≤ 120, candidate-report
  `execution_id`/`snapshot_id` ≤ 200 and `artifact_location` ≤ 500,
  `failure_reason` ≤ 2000, and the §7 declared `path` ≤ 200); it is moot for
  the ASCII-only name/schema bounds but applied uniformly.
- Errors are `{ "error": "snake_code", "message": "..." }` (403/404/409/400).
- Builder auth is the existing static Bearer `SNAPSHOT_BUILDER_AGENT_TOKEN`
  via `requireBuilder()` — unchanged; fencing is *in addition to* auth.
- Job kind allowlist is a CODE `as const` array (`JOB_KINDS` in
  `src/routes/snapshot_registry.ts` L37), **no DB CHECK on kind** — the new
  kind constant follows that pattern. Job **status** DOES have a DB CHECK
  (`capsule_snapshot_jobs_status_ck`) — relaxing it is a migration and is
  therefore **out of scope for PR-0** (see Non-goals).
- Endpoint family: `/v1/capsule-snapshots/jobs/...` (paths below are the
  names PR-1+ will mount; PR-0 fixes the names so both sides test against
  them, but mounts nothing).

---

## 0. ID formats

Prefix + ULID, matching the existing `job_${ulid()}` convention
(`snapshot_registry.ts` L540). All are opaque strings on the wire; the prefix
is a debugging/log affordance, not something receivers may parse for meaning.

| ID | Format | Minted by | Minted when | Notes |
|---|---|---|---|---|
| `job_id` | `job_<ULID>` | api | enqueue (existing) | unchanged |
| `submission_attempt_id` | `subatt_<ULID>` | api | **at enqueue** of the wizard job | 1:1 with the enqueue; stable for the attempt's lifetime (retries WITHIN the claim included). An interactive attempt is **never re-claimed** (design doc ADR-008 v3.1): lease expiry fails the attempt, and a subsequent claim serves a NEW attempt with a NEW `subatt_` |
| `worker_claim_id` | `claim_<ULID>` | api | **per claim generation** | a generation that fences duplicate/stale workers WITHIN an attempt. It is NOT a workspace-migration id, and there is no re-claim of an attempt: lease expiry on an interactive attempt fails the attempt (ADR-008 v3.1); the next claim carries a NEW `subatt_` + NEW `claim_` |
| `candidate_id` | `cand_<ULID>` | api | when a capture directive is issued | exactly **one candidate per `capture_epoch`** (epoch → candidate is 1:1); the id is delivered to the builder via the control channel and echoed back in the candidate report and acceptance |
| `verify_session_id` | `vsess_<ULID>` | api | when a verify session is created | independent resource, **1:N per candidate** (a candidate may be verified zero or many times) |
| `lease_token` | opaque string, no format promise (server mints ≥ 32 bytes of entropy, base64url) | api | per claim generation, alongside `worker_claim_id` | **Storage is hash-only** (server persists a hash, never the token — PR-1 concern; PR-0 doc-comments this on the type). Builders treat it as an opaque secret: never log it, never put it in URLs or request bodies — it travels ONLY in the `X-Ato-Lease-Token` header (§1) |
| `capture_epoch` | integer ≥ 0 | api | incremented per capture command | NOT an id, NOT a boolean, and **NOT a claim-fencing field** — a **monotonically increasing command cursor**. `0` = no capture has ever been requested on this claim's job. Per-endpoint role in §1.2 |
| `execution_id` / `snapshot_id` / `hardware_contract_id` etc. | existing formats (e.g. `hwc.…`, `asf.…`, `asc.…`) | — | — | unchanged; reused by reference |

---

## 1. Fencing and the epoch cursor

### 1.1 FENCING-4 (claim fencing, stated once, referenced everywhere)

> **FENCING-4**: Every builder-originated request after claim — control poll,
> lease renew, progress, hold-ready, candidate report, candidate acceptance,
> terminal ack — MUST carry the 4-tuple
> `{ job_id, submission_attempt_id, worker_claim_id, lease_token }`.
> The server compares all four against its authoritative row with **exact
> equality**. Any mismatch, or an expired lease, rejects the request with
> `409 { "error": "fenced", "message": "..." }` and the request has **no
> side effects**.

`capture_epoch` is **not** part of FENCING-4. It is a message-specific
command cursor whose role is defined per endpoint in §1.2. This is the B1
revision: a builder that has not yet observed the newest epoch is *behind*,
not *impostored* — a stale observation on the control poll must not fence an
otherwise-valid claim, or the builder could never learn the new epoch.

Transport of the tuple (uniform across ALL endpoints, GET and POST):

- `job_id`: always in the URL path (`/jobs/:job_id/...`). It is never
  repeated in the body.
- `lease_token`: always in the request header **`X-Ato-Lease-Token`**
  (case-insensitive per HTTP; canonical spelling as written). Never a query
  param, never a body field — this keeps the secret out of URLs, access
  logs, and body-logging/tracing pipelines (see D2 rationale in §11).
  Request-body schemas are `.strict()` and MUST **reject** a `lease_token`
  key appearing in the body (mandatory test on both sides).
- `submission_attempt_id`, `worker_claim_id`: top-level body fields on
  POSTs; query params on the control-poll GET.

Every message section below says "Fencing: FENCING-4" instead of re-listing
the fields.

### 1.2 Per-endpoint `capture_epoch` rules

| Endpoint | Epoch field carried | Server rule |
|---|---|---|
| Control poll (GET §3.3) | `observed_capture_epoch` (query, int ≥ 0) | **ACCEPT** `observed <= server_epoch` (stale observers are served the current state so they can catch up). **REJECT** only `observed > server_epoch` → `409 fenced` (a builder cannot have observed the future; treat as corrupt/forged state) |
| Lease renew (§3.2) | — (none) | FENCING-4 only; epoch plays no role |
| Progress (§3.4) | — (none) | FENCING-4 only; epoch plays no role |
| Hold-ready (§3.5) | — (none) | FENCING-4 only; epoch plays no role |
| Candidate report (§3.6) | `capture_epoch` (body, int ≥ 1) | **exact match** of `candidate_id` + the candidate's `capture_epoch` + active claim (FENCING-4). Mismatch → `409 fenced` |
| Candidate acceptance (§3.7) | `capture_epoch` (body, int ≥ 1) | **exact match** of path `candidate_id` + the candidate's `capture_epoch` + active claim (FENCING-4). Mismatch → `409 fenced` |
| Terminal ack (§3.8) | — (none) | FENCING-4 only; epoch plays no role |

### 1.3 Mandatory epoch contract tests (BOTH sides)

Encoded at schema/refinement level where applicable; where the rule is
server-side (comparison against the authoritative row), it is documented
here as a server rule and tested as such when routing lands (PR-1), with the
PR-0 schema tests proving the shapes admit/deny the right payloads:

| # | Scenario | Expected |
|---|---|---|
| (a) | server epoch advances 0→1; builder polls with `observed_capture_epoch=0` | `200` with `directive: "capture"`, `server_capture_epoch: 1` — the schema MUST allow the response epoch to differ from the observed epoch |
| (b) | builder polls with `observed_capture_epoch=2` while server epoch is `1` | `409 fenced` (observed > server) |
| (c) | candidate report with `capture_epoch=0` against a candidate whose epoch is `1` | reject (`409 fenced`; additionally `capture_epoch=0` is a schema reject on report/acceptance, whose epoch floor is 1) |
| (d) | candidate report with `capture_epoch=1` against a candidate whose epoch is `1` | accept |

---

## 2. Enums (exact wire strings)

```text
job kind        (existing + new): "recipe" | "dockerfile_import" | "oci_image_import"
                                  | "compose_import" | "interactive_capture"   ← NEW constant
job status      (existing + new): "queued" | "building" | "verifying" | "sealed"
                                  | "failed" | "holding"                        ← NEW constant
stage (coarse):                    "fetch" | "runtime" | "deps" | "build" | "launch"
                                  | "holding" | "quiescing" | "capturing" | "accepting"
failure_stage:                     any stage value, plus "capture_seal" | "acceptance"
control directive:                 "hold" | "capture" | "discard"
candidate status:                  "reported" | "verifying" | "accepted" | "rejected" | "expired"
acceptance status:                 "accepted" | "rejected"                      ← NEW (§3.7 body)
terminal ack reason:               "discarded" | "build_failed"
                                  | "acceptance_failed_source_lost" | "attempt_ended"  ← NEW (§3.8)
verify session status:             "pending" | "active" | "ended" | "failed" | "expired"
quiesce message type:              "quiesce" | "quiesced" | "unquiesce"
```

Notes:

- `"interactive_capture"` and `"holding"` are **defined but not wired** in
  PR-0: the kind is NOT added to `JOB_KINDS`, no enqueue accepts it, the
  builder does NOT advertise it in `supported_kinds`, and `"holding"` cannot
  be stored (DB CHECK unchanged; migration is PR-1+).
- **The legacy `"sealed"` job-terminal status is NOT used by
  `interactive_capture` jobs.** Candidate acceptance (§3.7) is a per-candidate
  operation and does not end the job; job termination goes through the wizard
  terminal ack (§3.8) with a `reason` from the enum above. The types enforce
  this: the wizard terminal-ack schema has no `"sealed"` member, and the
  shared ack schema refines `status: "sealed"` to be invalid when the job
  kind is `interactive_capture`.
- **`"lease_expired"` is NOT a terminal-ack reason — lease expiry is
  SERVER-OWNED.** The API's lease sweep transitions an expired attempt to
  `expired` and revokes its bindings; the builder observes `409 fenced` on
  its next renew/control call and tears down LOCALLY, without sending a
  terminal ack (an expired-lease ack is unsendable — FENCING-4 would `409`
  it, the lease being already dead). See §3.8 for the projection table.
- The job-kind list above is the **enqueue** kind vocabulary
  (`WIZARD_WIRE_JOB_KINDS` api-side; the `JOB_KIND_INTERACTIVE_CAPTURE`
  constant Rust-side), NOT the union of the kinds a builder advertises in
  `supported_kinds`: live builders also advertise `"source_materialize"`,
  which is not an enqueue kind and must never be rejected when PR-1 validates
  a claimed kind. Do NOT add `source_materialize` to the wire-kind enums on
  either side.
- `failure_stage` discrimination is diagnostic only on the wizard lane: a
  failure while sealing the captured filesystem/snapshot is `"capture_seal"`;
  an acceptance-time failure is `"acceptance"`. Coarse stages are for
  progress; on the wizard terminal ack the enum is an optional refinement of
  `reason`, never a substitute for it.
- Unknown kind on the builder continues to fail closed
  (existing `claim_kind` failure path), never guessed.

---

## 3. Builder-lane messages

All under the existing builder auth (`requireBuilder()`); zValidator-style
zod on the api side, serde structs on the builder side. All requests carry
`X-Ato-Lease-Token` (§1.1) — including the GET. All bodies are `.strict()` /
`deny_unknown_fields` and reject a body-level `lease_token` key.

**Null policy — optional fields are encoded by omission.** An absent optional
field is OMITTED from the JSON entirely; explicit `null` is NOT a legal
encoding of absence and is a schema reject on BOTH sides (the api's zod
`.optional()` admits only an absent key, never `null`; the Rust `Option`
fields reject `null` at parse). Emitters never serialize `null`. This is a
mandatory test on both sides, mirroring the strict-body `lease_token` test
(§1.1).

### 3.1 Claim response extension

`POST /v1/capsule-snapshots/jobs/claim` — request body unchanged
(`agent_id`, `capacity?`, `supported_kinds?`). The **per-job object** in the
response `jobs: [...]` array gains five fields, present iff the job kind is
`"interactive_capture"` (optional/`#[serde(default)]` otherwise, so existing
builders that never advertise the kind are untouched):

```json
{
  "jobs": [
    {
      "id": "job_01J1XY...",
      "capsule_id": "cap_...",
      "kind": "interactive_capture",
      "target_label": "web",
      "profile": "default",
      "wire_contract_version": "ato.submission-wizard-wire/v1",
      "submission_attempt_id": "subatt_01J1XY...",
      "worker_claim_id": "claim_01J1XZ...",
      "lease_token": "b64u-opaque-token",
      "lease_expires_at": "2026-07-22T09:15:00.000Z"
    }
  ]
}
```

| Field | Type | Req (for this kind) | Semantics |
|---|---|---|---|
| `wire_contract_version` | literal `"ato.submission-wizard-wire/v1"` | yes | required literal on both sides; any other value is a schema reject (fail-closed version gate) |
| `submission_attempt_id` | string `subatt_` | yes | fixed at enqueue; echoed in FENCING-4 |
| `worker_claim_id` | string `claim_` | yes | fresh per claim generation; echoed in FENCING-4 |
| `lease_token` | string, opaque | yes | secret; echoed in the `X-Ato-Lease-Token` header on every subsequent request; server stores hash only (PR-1). This claim response is the ONLY message in which the token appears in a JSON payload |
| `lease_expires_at` | string, ISO-8601 UTC | yes | lease deadline; builder must renew before this |

### 3.2 Lease renew

`POST /v1/capsule-snapshots/jobs/:job_id/lease/renew`

Header: `X-Ato-Lease-Token`. Fencing: FENCING-4. Epoch: none (§1.2).

Request body (strict; exactly these two fields):

```json
{ "submission_attempt_id": "subatt_01J1XY...",
  "worker_claim_id": "claim_01J1XZ..." }
```

Response `200`:

```json
{ "lease_expires_at": "2026-07-22T09:20:00.000Z" }
```

| Field | Type | Req | Semantics |
|---|---|---|---|
| `lease_expires_at` | ISO-8601 UTC | yes | new deadline. The `lease_token` is **stable within a claim generation** — renew extends expiry, it does not rotate the token. New token ⇔ new `worker_claim_id` only — and for an interactive job a new `worker_claim_id` only ever arrives with a NEW attempt (new `subatt_`), never as a re-claim of this one. |

Failure: `409 fenced` (includes "lease already expired" — an expired lease
cannot be renewed). Per ADR-008 v3.1 an interactive attempt is **never
re-claimed**: lease expiry marks the attempt expired/failed, and a subsequent
claim starts a NEW attempt from build, minting a NEW `subatt_` + NEW
`claim_`/token pair.

### 3.3 Control poll

`GET /v1/capsule-snapshots/jobs/:job_id/control?submission_attempt_id=...&worker_claim_id=...&observed_capture_epoch=N`

Header: `X-Ato-Lease-Token`. Fencing: FENCING-4.
Epoch rule (§1.2): `observed_capture_epoch` is the highest epoch the builder
has observed (`0` if none). The server ACCEPTS `observed <= server_epoch` and
REJECTS only `observed > server_epoch` with `409 fenced`. A stale observer is
served the current authoritative state — that is how it catches up.

Response `200`:

```json
{
  "directive": "capture",
  "server_capture_epoch": 3,
  "candidate_id": "cand_01J1Z0...",
  "hold_expires_at": "2026-07-22T09:45:00.000Z",
  "pause_permitted": true
}
```

| Field | Type | Req | Semantics |
|---|---|---|---|
| `directive` | `"hold" \| "capture" \| "discard"` | yes | `hold`: keep the session alive and keep polling. `capture`: perform capture for `server_capture_epoch`. `discard`: tear down without capturing; the attempt is over for this claim (terminal ack `reason: "discarded"`, §3.8). |
| `server_capture_epoch` | int ≥ 0 | yes | current authoritative epoch. With `directive: "capture"` it is ≥ 1 and names the command; the builder adopts it as its observed epoch. `0` ⇔ no capture ever requested. MAY exceed the request's `observed_capture_epoch` (test (a), §1.3). |
| `candidate_id` | string `cand_` | only when `directive: "capture"` | the pre-minted candidate for this epoch (1:1); builder echoes it in the candidate report and in the acceptance path |
| `hold_expires_at` | ISO-8601 UTC | yes when `directive: "hold"` | server-side hold deadline; after it, expect `discard` |
| `pause_permitted` | boolean | yes | **causality carrier for the quiesce contract (§5)**: the API sets this `true` for a given epoch only after it has received the proxy's `quiesced { epoch, inflight: 0 }` ack. The builder MUST NOT pause/freeze the guest for capture until it observes `pause_permitted: true` at the capture epoch. |

### 3.4 Progress

`POST /v1/capsule-snapshots/jobs/:job_id/progress`
Header: `X-Ato-Lease-Token`. Fencing: FENCING-4. Epoch: none (§1.2).

```json
{ "submission_attempt_id": "subatt_01J1XY...",
  "worker_claim_id": "claim_01J1XZ...",
  "stage": "deps" }
```

| Field | Type | Req | Semantics |
|---|---|---|---|
| `stage` | stage enum (§2, the 9 coarse values — NOT `capture_seal`/`acceptance`) | yes | coarse progress; server may surface to the wizard UI. Monotonic advance is not enforced on the wire (retries/restarts within a claim may repeat a stage). |

Response `200 {}`.

### 3.5 Hold-ready report

`POST /v1/capsule-snapshots/jobs/:job_id/hold-ready`
Header: `X-Ato-Lease-Token`. Fencing: FENCING-4. Epoch: none (§1.2).
Sent once when the app is up and the builder enters `holding`.

```json
{ "submission_attempt_id": "subatt_01J1XY...",
  "worker_claim_id": "claim_01J1XZ...",
  "builder_id": "builder-sugamo-1", "slot_id": "slot-3",
  "session_id": "sess_01J1Y9...", "guest_port": 8000 }
```

| Field | Type | Req | Semantics |
|---|---|---|---|
| `builder_id` | string 1..120 | yes | stable builder identity (same charset/limits as `agent_id`) |
| `slot_id` | string 1..120 | yes | slot on that builder hosting the held session |
| `session_id` | string 1..120 | yes | builder-local session identity for the held app |
| `guest_port` | int 1..65535 | yes | port inside the guest the app listens on |

**Deliberately absent (design doc ADR-004, SSRF)**: there is NO self-reported
upstream URL/host/address field, and the api MUST reject unknown fields here
(`.strict()`). The api derives the proxy upstream itself from
`(builder_id, slot_id, session_id, guest_port)` against its own registry of
builder ingress addresses — a builder can never point the proxy at an
arbitrary URL.

Response `200 {}`.

### 3.6 Candidate report

`POST /v1/capsule-snapshots/jobs/:job_id/candidates`
Header: `X-Ato-Lease-Token`. Fencing: FENCING-4.
Epoch rule (§1.2): body `capture_epoch` must **exactly equal** the epoch of
the candidate named by `candidate_id` (tests (c)/(d), §1.3). Sent after a
`capture` directive completes at seal (this message reports a **sealed**
candidate; a capture that fails before seal produces no candidate report —
with the source VM alive the attempt simply returns to `holding` and resumes
polling, and only a terminal condition goes through the ack §3.8).

```json
{ "submission_attempt_id": "subatt_01J1XY...",
  "worker_claim_id": "claim_01J1XZ...",
  "capture_epoch": 3,
  "candidate_id": "cand_01J1Z0...",
  "execution_id": "exec_01J1Z1...",
  "snapshot_id": "snap_01J1Z2...",
  "artifact_location": "r2://snapshots/cand_01J1Z0.../seal",
  "source_lost": false }
```

| Field | Type | Req | Semantics |
|---|---|---|---|
| `candidate_id` | string `cand_` | yes | must equal the id the control channel delivered for this epoch (server cross-checks epoch↔candidate 1:1) |
| `capture_epoch` | int ≥ 1 | yes | the epoch being reported; must exactly match the candidate's epoch (a report for a superseded epoch is rejected `409 fenced`) |
| `execution_id` | string, 1..200 | yes | **the canonical identity** of the captured execution (see §6 naming rule) |
| `snapshot_id` | string, 1..200 | yes | sealed snapshot produced for this candidate |
| `artifact_location` | string, 1..500 | yes | same semantics as the existing sealed-ack `artifact_location` |
| `source_lost` | boolean | yes | `true` ⇒ the live session died/was destroyed during or after capture (design doc ADR-012 `accepting_source_lost`); the candidate is still reportable but no further captures can come from this claim without a fresh launch |

Response `200 { "candidate_id": "cand_01J1Z0...", "status": "reported" }`
(candidate status enum §2).

### 3.7 Candidate acceptance (NEW endpoint — not a job-terminal ack)

`POST /v1/capsule-snapshots/jobs/:job_id/candidates/:candidate_id/acceptance`
Header: `X-Ato-Lease-Token`. Fencing: FENCING-4.
Epoch rule (§1.2): body `capture_epoch` must **exactly equal** the epoch of
the path `candidate_id`.

Reports the outcome of the acceptance run (disposable-restore validation,
ato#1088) for one candidate. **This does not terminate the job.** With the
source VM available, the attempt returns to `holding` after acceptance —
whether accepted (publisher may retake) or rejected (candidate discarded,
re-capture possible) — per the design doc §5 state machine and ADR-012. Only
the `accepting_source_lost` + acceptance-failure branch ends the attempt, and
that goes through the terminal ack (§3.8, `acceptance_failed_source_lost`).

```json
{ "submission_attempt_id": "subatt_01J1XY...",
  "worker_claim_id": "claim_01J1XZ...",
  "capture_epoch": 3,
  "status": "accepted",
  "acceptance_receipt": {
    "receipt_schema": "ato.snapshot-acceptance/v1",
    "receipt": { "any": "opaque payload — see envelope rule below" }
  } }
```

| Field | Type | Req | Semantics |
|---|---|---|---|
| `capture_epoch` | int ≥ 1 | yes | exact-match against the candidate's epoch |
| `status` | `"accepted" \| "rejected"` (acceptance status enum §2) | yes | outcome of the acceptance run |
| `acceptance_receipt` | versioned envelope (below) | yes when `status: "accepted"`; absent when `"rejected"` | evidence of the acceptance run |
| `failure_reason` | string ≤ 2000 | optional, only with `status: "rejected"` | human-readable reason (builder truncates at 1800) |

**Acceptance-receipt envelope (D3)** — the receipt is a versioned envelope,
NOT a pinned field list:

```json
{ "receipt_schema": "ato.snapshot-acceptance/v1", "receipt": { } }
```

- `receipt_schema`: required literal `"ato.snapshot-acceptance/v1"`.
- `receipt`: required **opaque JSON object**. Its payload schema is defined
  as a shared type in ato#1088 (post-Gate-0); until then BOTH sides validate
  ONLY the envelope (literal + "is an object"), never individual payload
  keys. Do NOT pin pre-Gate-0 field lists — the earlier 9-key required core
  from the seam round is superseded and its pinning tests are removed.
- The envelope is **strict on BOTH sides** (api `.strict()`, Rust
  `deny_unknown_fields`): payload keys live INSIDE `receipt`, never beside
  it — an unknown key next to `receipt_schema`/`receipt` (e.g.
  `execution_id`) is a schema reject. Strictness on the outer acceptance
  body does not extend to nested objects by itself, so the envelope pins its
  own strictness (mandatory test on both sides).

Response `200 { "candidate_id": "cand_01J1Z0...", "status": "accepted" }`
(acceptance status enum; the candidate's own status moves to
`accepted`/`rejected` per §2's candidate status enum).

### 3.8 Wizard terminal ack (restricted)

`POST /v1/capsule-snapshots/jobs/:job_id/ack` — the existing endpoint. For
`interactive_capture` jobs the body is the **wizard terminal-ack payload**
(discriminated by job kind; existing non-wizard acks are untouched):

Header: `X-Ato-Lease-Token`. Fencing: FENCING-4. Epoch: none (§1.2).

```json
{ "agent_id": "builder-sugamo-1",
  "submission_attempt_id": "subatt_01J1XY...",
  "worker_claim_id": "claim_01J1XZ...",
  "reason": "attempt_ended" }
```

(The absent optionals `failure_stage`/`failure_reason` are omitted — never
`null`; explicit `null` is a schema reject per the §3 null policy.)

| Field | Type | Req | Semantics |
|---|---|---|---|
| `agent_id` | string 1..120 (existing bounds) | yes | as the existing ack |
| `reason` | terminal ack reason enum (§2): `"discarded" \| "build_failed" \| "acceptance_failed_source_lost" \| "attempt_ended"` | yes | the ONLY legal job-terminal reasons for a wizard job. `discarded` = server directed discard; `build_failed` = build/boot never reached holding; `acceptance_failed_source_lost` = ADR-012 terminal branch (source lost AND acceptance failed); `attempt_ended` = orderly end of the interactive attempt (publisher done / session ended). **Lease expiry is NOT a reason here** — see the server-owned note + projection table below |
| `failure_stage` | failure_stage enum (§2: stages + `"capture_seal"` + `"acceptance"`) | optional | diagnostic refinement of a failure reason |
| `failure_reason` | string ≤ 2000 | optional | as existing (builder truncates at 1800) |

**Lease expiry is SERVER-OWNED — never a builder terminal ack.** An
`interactive_capture` attempt whose lease expires is swept by the API: the
sweep transitions the attempt to `expired` and revokes its bindings. The
builder observes `409 { "error": "fenced" }` on its next renew/control call
and tears down LOCALLY, WITHOUT sending a terminal ack. An expired-lease
terminal ack is unsendable — FENCING-4 would `409` it (the lease is already
dead) — and the sweep alone moves the attempt to `expired` (no builder ack
required). `"lease_expired"` is therefore absent from the reason enum. Server
enforcement (sweep + `409` on a dead lease) lands in PR-1; PR-0 pins the enum
+ this rule with a schema-level reject test on both sides.

Job-terminal projection of the reasons (plus the server-owned expiry path):

| Terminal condition | Job-terminal state | Owner |
|---|---|---|
| `discarded` (ack) | `ended` | builder ack |
| `attempt_ended` (ack) | `ended` | builder ack |
| `build_failed` (ack) | `failed` | builder ack |
| `acceptance_failed_source_lost` (ack) | `failed` | builder ack |
| lease expiry (no ack) | `expired` | server sweep (no builder ack) |

**The legacy `status: "sealed"` terminal ack is NOT used by
`interactive_capture` jobs** (§2 note). There is no `accepted_candidate_id`
and no receipt on the terminal ack — candidate acceptance is §3.7's endpoint
and is not job-terminal. Types enforce both: the wizard payload has no
`status`/`accepted_candidate_id`/`acceptance_receipt` members, and the shared
ack schema refines `"sealed"` invalid for this kind.

---

## 4. Verify sessions (types only)

A verify session is an independent resource, 1:N per candidate. PR-0 defines
only its wire object (returned by future wizard-facing routes, not builder
routes):

```json
{ "verify_session_id": "vsess_01J1Z5...",
  "candidate_id": "cand_01J1Z0...",
  "status": "active",
  "expires_at": "2026-07-22T10:00:00.000Z" }
```

| Field | Type | Req | Semantics |
|---|---|---|---|
| `verify_session_id` | string `vsess_` | yes | its own lifecycle; deleting/expiring it never mutates the candidate |
| `candidate_id` | string `cand_` | yes | parent candidate |
| `status` | verify session status enum (§2) | yes | `pending → active → ended`, or `failed`/`expired` |
| `expires_at` | ISO-8601 UTC | yes | hard deadline |

---

## 5. api ⇄ proxy quiesce contract (internal, types only)

Three messages, discriminated by `type` (quiesce message type enum §2).
Transport (service binding / DO) is PR-2; PR-0 fixes the shapes:

```json
{ "type": "quiesce",   "epoch": 3 }
{ "type": "quiesced",  "epoch": 3, "inflight": 0 }
{ "type": "unquiesce", "epoch": 3 }
```

| Message | Fields | Semantics |
|---|---|---|
| `quiesce` | `epoch` int ≥ 1 | api → proxy: stop admitting new requests to the held session's upstream for this capture epoch; drain in-flight |
| `quiesced` | `epoch`, `inflight` (must be `0`) | proxy → api: drain complete. **Only after receiving this ack for epoch N may the api set `pause_permitted: true` in control responses at epoch N** (§3.3) — the causality is encoded in that ordering, and `inflight` is fixed at literal 0 (an ack with traffic still in flight is not a valid message). |
| `unquiesce` | `epoch` | api → proxy: resume proxying (after capture completes or is aborted) |

**Drain timeout is fail-closed**: if the proxy cannot reach `inflight: 0`
within the drain window, the api never sets `pause_permitted`, aborts the
capture for that epoch (the attempt returns to `holding` per the design-doc
ADR-007 fail-closed rule; a later epoch may retry), and sends `unquiesce`.
The system NEVER force-captures under live traffic.

---

## 6. Publish-semantics naming (referenced types only)

- `publisher_verified_snapshot_id` — attestation: the snapshot the publisher
  personally verified.
- `preferred_snapshot_id` — optional routing hint.
- **`execution_id` is the canonical identity.** No field in any wizard
  message may be named to imply a snapshot is the canonical launch key
  (banned shapes: `launch_snapshot_id`, `canonical_snapshot_id`,
  `snapshot_launch_key`). Reviewers should treat any new `*snapshot_id`
  field name as needing one of the two names above or a rename.

---

## 7. capsule.toml declaration schema (`[cache.*]` / `[state.*]`)

Parse types + validation only (in `wizard_wire.rs`, using `toml = "0.8"` — the
workspace already pins toml 0.8; PR-0 adds it to the snapshot-builder crate,
i.e. `Cargo.toml` + an existing lock entry); NOT consulted by any build path in
PR-0.

Grammar — **paths are GUEST-ABSOLUTE** (D1; design doc §1.2: the declared
surfaces are absolute guest filesystem paths like `/var/cache/example-model`
and `/data`):

```toml
# Baked into the captured snapshot (declared cache surface)
[cache.<name>]
path = "<guest-absolute path>" # required, e.g. "/var/cache/example-model"
capture = "include"            # required; "include" | "exclude"

# Never baked; runtime durable state (restore-time binding, per v1.6 rule)
[state.<name>]
path = "<guest-absolute path>" # required, e.g. "/data"
snapshot = "exclude"           # required; ONLY "exclude" is legal for state
schema = "<identifier>"        # required; free-form id, 1..60 chars,
                               #   [a-z0-9_.-], e.g. "sqlite", "kv-dir", "1"
```

Rules:

- `<name>`: `[a-z0-9_-]{1,40}`, unique across cache ∪ state.
- **Unknown keys INSIDE a `[cache.*]`/`[state.*]` declaration are rejected on
  BOTH sides** (api `.strict()`, Rust `deny_unknown_fields`) — only unknown
  tables *elsewhere in the manifest* are ignored. The same declaration must
  get one verdict on both sides.
- **Top-level envelope asymmetry is intentional (division of labor).** The
  Rust parser (`parse_capture_declarations`) reads the *whole capsule.toml*
  and ignores unknown top-level tables; the api's `captureDeclarationsSchema`
  is `.strict()` and rejects unknown top-level keys. This is not a divergence:
  the JSON projection the api validates contains ONLY the `cache`/`state`
  keys, produced by extracting those two tables from the manifest — never by
  serializing the whole manifest. Both sides agree on the declaration set;
  only the *envelope* differs (a full manifest Rust-side vs. the two-table
  projection api-side).
- `path` (identical validator on BOTH sides):
  - MUST start with a **single** `/` (so a leading `//` is rejected);
  - no scheme prefix (a path failing the leading-`/` rule, e.g.
    `file:///models` or `r2://x`, is rejected by that rule; additionally any
    `:` before the first `/` is rejected as a scheme);
  - no backslashes anywhere;
  - no `//` anywhere and no empty segments;
  - no `.` or `..` segments;
  - no trailing `/` (an empty last segment);
  - non-empty beyond the leading `/` (bare `"/"` is rejected);
  - length ≤ 200 **UTF-16 code units** (bounds unchanged from the draft).
- Two declarations may not have identical paths; **nesting between ANY two
  declarations** — cache↔cache, cache↔state, state↔state alike — is a
  validation error (any ancestor/descendant relation on the declared paths,
  not only across sections). Both the collision and nesting checks are
  computed on the **absolute** paths exactly as declared (no normalization
  pass exists, because every input that would need normalizing — `//`,
  `.`/`..`, trailing `/` — is already rejected above). Nested surfaces
  (longest-prefix precedence etc.) are deferred to a future contract version.
- **Capture-refusal domain**: any filesystem write at capture time that is
  outside every declared `[cache.*]`/`[state.*]` path is grounds for the
  capture to be refused (enforced later; the parser only produces the
  declared-path set + a `refusal_domain` complement marker in PR-0).

Valid example (matches design doc §1.2):

```toml
[cache.model]
path = "/var/cache/example-model"
capture = "include"

[cache.pip]
path = "/root/.venv"
capture = "exclude"

[state.data]
path = "/data"
snapshot = "exclude"
schema = "1"

[state.db]
path = "/var/lib/app/data/app.db"
snapshot = "exclude"
schema = "sqlite"
```

Invalid examples (each is a distinct test case):

```toml
[cache.rel]
path = ".venv"              # ERROR: not absolute (must start with "/")

[cache.doubleslash]
path = "//var/cache"        # ERROR: leading "//" (not a SINGLE "/")

[cache.scheme]
path = "file:///models"     # ERROR: scheme (also fails the leading-"/" rule)

[cache.up]
path = "/var/../etc"        # ERROR: ".." segment

[cache.slash]
path = "/data/"             # ERROR: trailing '/' (empty segment)

[cache.backslash]
path = "/var\\cache"        # ERROR: backslash

[cache.root]
path = "/"                  # ERROR: bare root

[cache.maybe]
path = "/var/cache"
capture = "sometimes"       # ERROR: not "include"|"exclude"

[state.data2]
path = "/data2"
snapshot = "include"        # ERROR: state is never snapshot-included
schema = "sqlite"

[state.nodecl]
path = "/srv/data"
snapshot = "exclude"        # ERROR: missing required `schema`

[cache.dup]
path = "/data"              # ERROR: duplicate path with [state.data]
capture = "include"

[cache.nest]
path = "/data/cache"        # ERROR: nested under state path "/data" (cross-section)
capture = "include"

[cache.vendor]
path = "/var/cache"         # (with [cache.session] below) two cache decls…
capture = "include"

[cache.session]
path = "/var/cache/session" # ERROR: nested under [cache.vendor] — same-section
capture = "include"         #        nesting is ALSO rejected (any two decls)
```

---

## 8. Explicit NON-goals for PR-0

1. **No routes wired**: none of §3's endpoints are mounted in ato-api;
   `src/index.ts` untouched. That includes the new acceptance endpoint §3.7.
2. **No migrations**: no drizzle/schema.ts changes, no new tables, no change
   to the `capsule_snapshot_jobs_status_ck` CHECK (so `"holding"` is a code
   constant that cannot yet be persisted).
3. **No allowlist changes**: `JOB_KINDS` does not gain
   `"interactive_capture"`; enqueue, claim matching, and the builder's
   advertised `supported_kinds` are all unchanged.
4. **No builder behavior**: `snapshot-builder` gains `wizard_wire.rs`
   types/tests only — no polling loop, no hold/quiesce/capture execution,
   no new ack paths taken at runtime.
5. **No proxy changes**: §5 shapes are types in ato-api only.
6. **No lease-token storage**: hashing/persistence of `lease_token` is PR-1;
   PR-0 carries it only as a doc-commented opaque string type.
7. **No acceptance-receipt payload schema**: only the D3 envelope is
   validated; the `receipt` payload type arrives with ato#1088.
8. **No capsule-crate changes**: TOML schema lives in snapshot-builder
   (local-wire-structs precedent), `crates/capsule` untouched.
9. Branch bases per survey: ato-api off `main`, ato off `nightly`. No
   Co-Authored-By trailers.

---

## 9. Seam checklist (for the seam-check agent)

Both implementations MUST use these exact snake_case wire names. Any drift
is a seam failure.

**Wire version**: `wire_contract_version` =
`"ato.submission-wizard-wire/v1"` (required literal in the claim extension).

**FENCING-4 transport** (every builder message): path `job_id`, header
`X-Ato-Lease-Token`, body/query `submission_attempt_id` +
`worker_claim_id`. A `lease_token` key in ANY request body is a strict-mode
reject (tested). Control GET query params: `submission_attempt_id`,
`worker_claim_id`, `observed_capture_epoch`.

**Null policy** (§3): optional fields are OMITTED when absent. An explicit
`null` on any optional — control response `candidate_id`/`hold_expires_at`,
acceptance `acceptance_receipt`/`failure_reason`, terminal ack
`failure_stage`/`failure_reason` — is a schema reject on BOTH sides
(mandatory test, mirroring the `lease_token` strict-body test).

**Epoch fields**: `observed_capture_epoch` (poll request, ≥ 0),
`server_capture_epoch` (poll response, ≥ 0), `capture_epoch` (candidate
report + acceptance bodies, ≥ 1, exact-match vs candidate). No epoch field on
renew / progress / hold-ready / terminal ack.

**Claim response extension**: `wire_contract_version`,
`submission_attempt_id`, `worker_claim_id`, `lease_token`,
`lease_expires_at`.

**Renew response**: `lease_expires_at`.

**Control response**: `directive`, `server_capture_epoch`, `candidate_id`,
`hold_expires_at`, `pause_permitted`.

**Progress**: `stage`.

**Hold-ready**: `builder_id`, `slot_id`, `session_id`, `guest_port`
— and the ABSENCE of any upstream URL field (strict object).

**Candidate report**: `candidate_id`, `capture_epoch`, `execution_id`,
`snapshot_id`, `artifact_location`, `source_lost`.
Candidate report response: `candidate_id`, `status`.

**Candidate acceptance** (`POST .../candidates/:candidate_id/acceptance`):
`capture_epoch`, `status` (`"accepted"|"rejected"`), `acceptance_receipt`
(envelope: `receipt_schema` literal `"ato.snapshot-acceptance/v1"` +
`receipt` opaque object; the envelope is STRICT on both sides — an unknown
key beside those two rejects), `failure_reason`.
Acceptance response: `candidate_id`, `status`.

**Wizard terminal ack**: `agent_id`, `reason`
(`"discarded"|"build_failed"|"acceptance_failed_source_lost"|"attempt_ended"`),
`failure_stage`, `failure_reason` — and the ABSENCE of `status: "sealed"` /
`accepted_candidate_id` / any receipt for `interactive_capture` jobs. Lease
expiry is server-owned (sweep → `expired`, no builder terminal ack);
`"lease_expired"` is a reason-enum reject on both sides.

**Quiesce messages**: `type`, `epoch`, `inflight`.

**Verify session object**: `verify_session_id`, `candidate_id`, `status`,
`expires_at`.

**Enum strings**: exactly as §2 (including `"interactive_capture"`,
`"holding"`, the 9 stages, `"capture_seal"`, `"acceptance"`,
`"hold"/"capture"/"discard"`, candidate + verify-session statuses,
acceptance statuses, terminal-ack reasons,
`"quiesce"/"quiesced"/"unquiesce"`).

**TOML keys**: table names `cache`, `state`; keys `path`, `capture`,
`snapshot`, `schema`; values `"include"`, `"exclude"`; paths GUEST-ABSOLUTE
per §7. Nesting between ANY two declarations (cache↔cache, cache↔state,
state↔state) is rejected on both sides.

**ID prefixes**: `job_`, `subatt_`, `claim_`, `cand_`, `vsess_` (+ opaque
`lease_token`, integer epochs).

**Error envelope**: `409 { "error": "fenced", "message": ... }` for any
FENCING-4 or epoch-rule violation; standard `{ error, message }` elsewhere.

**Mandatory epoch tests**: the four cases of §1.3 exist on BOTH sides
(schema/refinement level where applicable; otherwise asserted as documented
server rules when routing lands).

---

## 10. Relationship to the design doc

The architecture design doc (PWA submission wizard v3.1,
`apps/ato-pwa/claudedocs/submission-wizard-architecture.md` — non-normative
for the wire) is the source for: §1.2 declared-surface semantics (absolute
guest paths), §5 attempt/candidate state machines (`holding`,
`accepting_source_available` / `accepting_source_lost`), ADR-007 (quiesce
handshake), ADR-008 v3.1 (lease/fencing, no re-claim), ADR-012 (source-lost
branch). Where this contract names a state or transition, it is that state
machine's wire projection; the wire never invents transitions.

---

## 11. CHANGELOG — revisions from the pre-review draft

The pre-review draft (scratchpad `wizard-pr0-wire-contract.md`, now a
non-normative stub) differed as follows. Reviewer blockers B1–B3, decisions
D1–D3:

- **[B1] FENCING-5 → FENCING-4 + epoch as command cursor.** The draft fenced
  every message on a 5-tuple including `capture_epoch`, which made a builder
  polling with a stale (but honest) epoch indistinguishable from a stale
  claim — it could never learn the new epoch. Claim fencing is now the exact
  4-tuple `{job_id, submission_attempt_id, worker_claim_id, lease_token}`;
  the epoch is message-specific: control poll carries
  `observed_capture_epoch` and is accepted when `observed <= server`
  (rejected only when ahead), renew/progress/hold-ready/ack carry no epoch,
  candidate report and acceptance exact-match the candidate's epoch. Four
  mandatory contract tests added (§1.3).
- **[B2] Candidate acceptance decoupled from job termination.** The draft
  overloaded the legacy ack (`status: "sealed"` + `accepted_candidate_id` +
  receipt) as both acceptance and job end, which contradicts the state
  machine (acceptance with a live source returns to `holding`; the publisher
  may retake). New endpoint
  `POST /jobs/:job_id/candidates/:candidate_id/acceptance` (§3.7); the
  terminal ack is restricted to the reason enum
  `discarded | build_failed | acceptance_failed_source_lost | attempt_ended`
  (§3.8) and `"sealed"` is never used by `interactive_capture` jobs. (The
  round-1 draft also listed `lease_expired`; the round-2 fix below removed it.)
- **[B3] SSOT moved into the ato repo.** The contract previously lived only
  in a session scratchpad — unversioned and invisible to reviewers of either
  implementation. It now lives at
  `docs/contracts/SUBMISSION_WIZARD_WIRE_V1.md` on the ato PR branch, with a
  fail-closed `wire_contract_version` literal
  (`"ato.submission-wizard-wire/v1"`) carried in the claim extension and
  pinned as a constant on both sides.
- **[D1] TOML paths are guest-absolute.** The draft required *relative*
  paths, which had the design doc backwards (§1.2 examples are
  `/var/cache/example-model`, `/data` — the declarations name guest
  filesystem surfaces, and there is no defined base to resolve a relative
  path against). §7 now requires a single leading `/` and forbids
  scheme/backslash/`//`/empty segments/`.`/`..`/trailing `/`; bounds
  unchanged; collision/nesting computed on the absolute paths; all examples
  and fixtures updated.
- **[D2] Lease token in header on ALL endpoints.** The draft put
  `lease_token` in POST bodies (header on the one GET only). Bodies are
  routinely logged/traced; the token is a bearer secret. It now travels
  exclusively in `X-Ato-Lease-Token`; strict bodies reject a `lease_token`
  key (tested); the only JSON appearance of the token is the claim response
  that mints it. Bodies carry only the remaining fencing fields
  (`submission_attempt_id`, `worker_claim_id`; `job_id` stays in the path).
- **[D3] Acceptance receipt is a versioned envelope.** The draft pinned the
  live sealed-receipt's 9 required keys, freezing a pre-Gate-0 field list
  into the wizard contract. Replaced by
  `{ receipt_schema: "ato.snapshot-acceptance/v1", receipt: <opaque
  object> }`; the payload schema arrives as a shared type in ato#1088; both
  sides validate only the envelope, and the 9-key pinning tests are removed.

Post-review seam fixes:

- **[Seam fix] Explicit `null` is not absence.** The §3.8 example carried
  `"failure_stage": null, "failure_reason": null` and got TWO verdicts on
  the seam: the Rust side parsed the nulls into its `Option`s while the
  api's zod `.optional()` rejects explicit `null` (it admits only an absent
  key) — a builder conforming to the printed example would be 400'd.
  Pinned: optionals are encoded by omission (§3 null policy), the example
  drops the nulls, the Rust optionals (control response
  `candidate_id`/`hold_expires_at`, acceptance
  `acceptance_receipt`/`failure_reason`, terminal ack
  `failure_stage`/`failure_reason`) now reject explicit `null` at parse,
  and the mandatory explicit-null test exists on both sides.
- **[Seam fix] Acceptance-receipt envelope is strict on both sides.**
  `deny_unknown_fields` on the outer acceptance body does not propagate to
  nested serde structs, so the Rust envelope accepted
  `{receipt_schema, receipt, extra}` that the api's `.strict()` envelope
  rejects — a strict-body divergence inside a builder→api request body
  (§1.1/§3 mandate all bodies strict). The envelope now pins its own
  strictness (`deny_unknown_fields`), with the unknown-envelope-key test
  mirrored on the Rust side.

Round-2 review fixes:

- **[Round-2] `lease_expired` removed — lease expiry is server-owned.** The
  round-1 terminal-ack reason `lease_expired` was unsendable: by the time the
  builder would report it, the lease is already dead, so FENCING-4 `409`s the
  ack. The reason enum is now
  `discarded | build_failed | acceptance_failed_source_lost | attempt_ended`;
  lease expiry is handled entirely server-side — the sweep transitions the
  attempt to `expired` and the builder tears down locally on `409 fenced`,
  sending no terminal ack (§3.8 server-owned note + projection table). Pinned
  by a schema-level `"lease_expired"` reject test on both sides.
- **[Round-2] All nesting between declarations is forbidden.** §7 rejected
  only cross-section nesting; two same-section declarations in an
  ancestor/descendant relation (e.g. two `[cache.*]` at `/var/cache` +
  `/var/cache/session`) slipped through. The rule now rejects nesting between
  ANY two declarations (cache↔cache, cache↔state, state↔state). Nested
  surfaces (longest-prefix precedence etc.) are deferred to a future contract
  version. Both validators, the §7 rule text + invalid examples, and the
  fixtures are updated — the round-1 same-section-accept tests are flipped to
  reject on both sides.
