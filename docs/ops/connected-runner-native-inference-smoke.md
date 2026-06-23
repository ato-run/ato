# Connected Runner native-inference smoke (#754 gate)

A repeatable, on-demand smoke that verifies a native-inference capsule
(`ato-run/local-llm-chat`) runs on a **Connected Runner** through the
`#149`/`#763` contract. This is the **final gate for #754** (Store/community
registration) — register the capsule only after this is GREEN.

It makes no production changes: it dispatches **one** run and stops it.

- Script: [`scripts/smoke-connected-native-inference.sh`](../../scripts/smoke-connected-native-inference.sh)
- Manual workflow: `.github/workflows/connected-native-inference-smoke.yml`
  (`workflow_dispatch` only — never runs on push/PR)

## What it proves

```
POST /v1/runs (external-runner)            ── operator session
  → ato-api mints a run_capsule lease, runtime="native-inference"      (ato-api #149)
  → dispatch is GATED on the runner advertising `native-inference`
      (a 409 runner_capability_required here = the runner does NOT advertise it)
→ runner claims the lease and runs `ato run <run_ref> -y` WITHOUT --sandbox  (ato #763)
→ run reaches ready → /health 200 → /v1/chat/completions works
→ ato stop / cleanup succeeds
```

### Two evidence tiers

| Tier | Needs | Proves |
|------|-------|--------|
| **API** (always) | operator session | dispatch accepted (⇒ runner advertises `native-inference`), run reached **ready**, `/health` 200, completion, clean stop. A native-inference capsule forced into `--sandbox` would **fail**, so a GREEN ready+completion is itself strong evidence the host path ran. |
| **Runner-SSH** (recommended) | SSH to the runner host | the **direct** proof: `ato ps --json` `runtime=host`, `/proc/<pid>/cmdline` shows `ato run <ref> -y` with **no `--sandbox`**, engine log shows llama.cpp/Vulkan. |

### Observability caveats (from endpoint recon)

- **`lease.command.runtime` is NOT exposed by the operator API.** It is fetched by
  the runner via `GET /v1/runners/:id/leases/next` (runner bearer token) and is
  not on `GET /v1/runs/:id` or the run list. The direct runtime/argv proof
  therefore needs **runner-host access** (the SSH tier). The API tier proves the
  runtime *decision* by its effect (the run runs host and serves).
- **The `native-inference` runner capability is NOT on `GET /v1/runners`**
  (`serializeRunner` omits `supported_lease_kinds`). The smoke confirms it
  **indirectly**: a successful `POST /v1/runs` for a native-inference recipe is
  the #149 gate passing (a non-advertising runner returns `409
  runner_capability_required`).
- Optional follow-up (cleaner future smokes, out of scope here): surface
  `supported_lease_kinds` on `serializeRunner`, and `command.kind`/`runtime` on
  the owner run-detail DTO.

## Prerequisites (the #754 gates — all required for a real run)

1. **ato-api `#149` deployed** to the target API (`staging` is deployed; **prod is
   not yet** — do not deploy prod without explicit approval).
2. **A Connected Runner on a `#763`-bearing 0.7.x build**, enrolled under the
   operator account, that **advertises `native-inference`** — i.e. its host passes
   `ato doctor native-inference` (the runner appends the capability only when ready).
3. **`local-llm-chat` registered** as a resolvable Store/community capsule
   (this is #754 itself — keep it deferred until the smoke is otherwise ready;
   the smoke fails fast with a clear message if the ref does not resolve).

> Until 2 and 3 exist, the smoke **cannot complete** — that is expected. It is
> kept ready to run the moment the gates clear. Staging today has #149 but no
> #763 runner and no registered capsule, so a staging run stops at dispatch.

## Run it

```bash
export ATO_API_BASE="https://staging.api.ato.run"   # prod: https://api.ato.run
export ATO_SESSION="<operator better-auth session token>"
export ATO_RUNNER_ID="<connected runner id, owned + online>"
export ATO_CAPSULE_REF="community/local-llm-chat"    # must be registered (#754)
# Optional — the direct --sandbox-absent proof:
export ATO_RUNNER_SSH="ubuntu@<runner-host>"
export ATO_RUNNER_SSH_KEY="$HOME/.ssh/<key>"          # if not in the agent

apps/ato/scripts/smoke-connected-native-inference.sh
```

Required inputs / secrets:

| Var | Required | Notes |
|-----|----------|-------|
| `ATO_SESSION` | yes | operator session token — **secret**, redacted from all evidence |
| `ATO_RUNNER_ID` | yes | the target runner (owned by the session account) |
| `ATO_API_BASE` | no | default staging; set prod only after approval |
| `ATO_CAPSULE_REF` | no | default `community/local-llm-chat` |
| `ATO_RUNNER_SSH`(`_KEY`) | no | enables the direct argv/runtime/engine-log proof |

Evidence is written **redacted** to `./smoke-evidence/` (`summary.txt`,
`dispatch.json`, `runner-argv.txt`, `engine-log.txt`, `completion.json`).
Tokens, bearer/cookie/authorization values, URL `?token=` params, and
`ghp_`/`ato_rnr_` secrets are scrubbed (the session token is matched literally,
so token formats with `+`/`/`/`.` are handled).

> **Re-running:** the runner allows one active run at a time. If a prior run was
> left active (e.g. a stop that only WARNed), the next dispatch can return `409`
> `single_active` / `capacity_exhausted` — the script reports this clearly and
> exits non-zero. Stop the dangling run (`POST /v1/runs/<id>/stop`) before re-running.

## Expected GREEN output

```
✓ API /health = 200
✓ session valid (operator: …)
✓ runner agent_version=0.7.x …
✓ dispatch 201 → run <id> (lease <id>)
✓ INDIRECT capability proof: dispatch accepted ⇒ runner advertises native-inference
✓ argv has NO --sandbox (native-inference dispatched host): ato run community/local-llm-chat@… -y
✓ ato ps runtime=host
✓ engine log shows llama.cpp/Vulkan
✓ run reached ready
✓ /health = 200
✓ completion: ready
✓ run stopped — cleanup acked
GREEN — native-inference ran on the Connected Runner via the #149/#763 contract.
```

Exit code `0` = GREEN (no failures and the run reached ready); `1` = not green.

## After GREEN

1. Update **#754** with the evidence (redacted) and unblock Store/community
   registration of `local-llm-chat`.
2. Until then **#754 stays blocked** on: prod `#149`, a `#763` runner advertising
   `native-inference`, and this smoke GREEN.

## Staging-first

Run against staging first if a staging runner is available. Staging exercises the
**same** dispatch path (`#149` is deployed there). If staging has no
`#763`/native-inference runner, the smoke stops at the dispatch step with a clear
`runner_capability_required` / `runner not found` message — that is the documented
missing piece, not a harness defect.
