# Native-inference production rollout plan (#754 unblock)

Planning/checklist for taking native-inference on Connected Runners to production
and unblocking #754. **Nothing here is executed** until the go/no-go decisions
below are made. Investigated 2026-06-23; commit/version refs are point-in-time —
re-verify before executing.

## Go / No-Go — **NO-GO as originally sequenced**; 3 decisions required first

The original gate order ("deploy → roll runner → smoke → *then* register") is not
executable as-is. Three blockers (all anticipated stop-conditions) need a decision:

### Blocker 1 — a prod deploy of #149 is NOT isolated (HIGH)
Prod ato-api is at `593a0d9` (#34/#128, deployed 2026-06-19, Worker version
`e578e6db`). `main` HEAD is `e41ed12` (#149). `wrangler deploy --env production`
ships **all 12 commits**, so it promotes **11 unrelated** ones — mostly
Managed-Cloud billing/entitlement/cancel (#140–#147), diagnostics ingest/seal
(#38/#129), OCI-image web target (#141), checkout-resume (#142) — that are
currently live **only on staging**, plus migration **0075** (from #140).

- **Decision A** — review + approve promoting all 11 (they are staging-tested) and
  deploy `main`.
- **Decision B (recommended)** — isolate #149: `git cherry-pick e41ed12` onto
  `593a0d9` on a throwaway branch and deploy **that**, decoupling the
  native-inference rollout from the billing/entitlement promotion.

### Blocker 2 — no released runner has #763 (MEDIUM)
v0.7.0 does **not** contain #763 (`git merge-base --is-ancestor b62ae5a5 v0.7.0`
→ false), and no pre-built nightly artifact is new enough (latest
`nightly-v0.5.5-20260618.6`, `release-nightly` @ Jun 19 — both predate #763/#764
on 2026-06-23). The first runner must run a binary built from current `nightly`
(`8aa18f1c`, has #763 + #764).

- **Chosen first runner**: **OCI box `oci-linux-test`** — Oracle ARM Linux (Ubuntu
  24.04, `aarch64`), **CPU-only** (no NVIDIA/Vulkan), 17 GiB RAM / 243 GB free,
  cargo 1.96.0 + a `~/ato-run` checkout already present. Predicted
  `ato doctor native-inference` = **ready** (platform OK via `ubuntu-arm64`
  prebuilt; acceleration = Warn "no GPU → CPU", which is **not** a Fail).
- **CPU is sufficient** for the first smoke (`local-llm-chat` default `chat`
  target = Qwen2.5-1.5B Q4_K_M, ~1.1 GB). `chat-vulkan` (GPU) = a **second** smoke
  on a GPU host, out of scope here.
- ⚠️ The OCI binaries are stale/inconsistent (`ato` 0.5.5, `ato-runner-bin` 0.6.1)
  and a runner is **already serving on the stale binary** (cannot advertise
  native-inference) — it must be stopped/replaced. Prior OCI history: `ato login`
  has overwritten enrollment creds — re-enroll carefully.

### Blocker 3 — `local-llm-chat` registration is a PREREQUISITE for the smoke (HIGH)
The smoke cannot trigger native-inference dispatch until `local-llm-chat` is a
**registered** capsule whose `capsule_source_recipes.recipe_toml` literally
contains `runtime = "native-inference"` — that is the only path to
`isNativeInference=true` (`runs.ts:1004-1071`). A github `source:` ref **bypasses**
this and 404s in `parseCapsuleRef` first. So **#754's registration must happen
before the smoke**, not after GREEN — the "register after GREEN" order is
impossible (chicken-and-egg).

- **Resolution**: register first; the smoke then **validates** the registered
  capsule. Strongly recommend a **staging rehearsal** (register + smoke on staging)
  before prod.

---

## Corrected rollout sequence (execute only after the 3 decisions)

```
0. DECIDE: bundle vs isolate #149 (Blocker 1); accept OCI build (Blocker 2);
           accept register-before-smoke (Blocker 3)
1. (recommended) STAGING rehearsal: register local-llm-chat on staging →
   enroll OCI to staging → #764 smoke on staging → GREEN
2. ato-api #149 → PROD (isolated cherry-pick, or reviewed main bundle)
3. verify prod health
4. build nightly on OCI → replace binary → ato doctor native-inference → re-enroll → serve
5. verify the runner advertises native-inference
6. register local-llm-chat in PROD store (#754 publish step)
7. run #764 smoke against PROD
8. if GREEN → #754 unblocked
```

## Step-by-step (exact commands)

### 1. ato-api production deploy
Run from `apps/ato-api`. Secrets are already on the live Worker (`FLY_API_TOKEN`,
`MANAGED_RUNNER_AGENT_TOKEN`, `STRIPE_*`, `TVM_*`, `TURNSTILE_*`, …) — a code deploy
does **not** reset them; **#149 needs no new secret and no migration**.

```bash
# migration 0075 is from #140 (additive: price_cents/currency on cloud_plans),
# NOT #149. Apply it only if you are also promoting #140's paywall code (Decision A).
npm run db:migrate:remote:production           # = wrangler d1 migrations apply ato-store-db-prod --remote --env production

# Decision A (promote main): from apps/ato-api on main
npx wrangler deploy --env production            # Worker "ato-store", D1 ato-store-db-prod
# Decision B (isolate #149): on a branch = e41ed12 cherry-picked onto 593a0d9
#   git checkout -b rollout/149-isolated 593a0d9 && git cherry-pick e41ed12 && npx wrangler deploy --env production
```

Health checks:
```bash
curl -s https://api.ato.run/health                       # → 200 {"status":"ok","service":"ato-store",...}
curl -s "https://api.ato.run/v1/search?sort=recommended" # → 200, ranked catalogue
```

### 2. First Connected Runner (OCI, nightly build)
```bash
ssh oci-linux-test
git -C ~/ato-run fetch origin && git -C ~/ato-run checkout nightly && git -C ~/ato-run pull   # must include b62ae5a5 (#763)
cd ~/ato-run/apps/ato && ~/.cargo/bin/cargo build --release -p cli
cp ~/.local/bin/ato ~/.local/bin/ato.bak.0.5.5 2>/dev/null || true
cp target/release/ato ~/.local/bin/ato
~/.local/bin/ato doctor native-inference                 # expect: ready (CPU; acceleration Warn, no Fail)
# stop the stale runner serve, then re-enroll + serve (creds handled carefully):
pkill -f "runner serve" || true
~/.local/bin/ato runner login --headless                 # operator approves the printed URL
~/.local/bin/ato runner serve &                          # advertises native-inference ONLY if doctor was ready at startup
```

### 3. Verify the runner advertises native-inference
`native-inference` is **not** on `GET /v1/runners` (serializeRunner omits
`supported_lease_kinds`). Confirm **indirectly**: the #764 smoke's dispatch
succeeding (not `409 runner_capability_required`) IS the proof. (DB-admin could
read `runner_devices.supported_lease_kinds_json` directly if needed.)

### 4. Register `local-llm-chat` (the #754 publish step)
```bash
# store-apply → approve → the SOLE writer of capsule_source_recipes:
node apps/ato-api/scripts/internal/publish-submission.mjs <submission_id> --db ato-store-db-prod
# the published recipe_toml MUST contain a target with: runtime = "native-inference"
```
After this `resolveCapsuleRef('community/local-llm-chat')` succeeds and the recipe
regex matches → lease carries `runtime="native-inference"`.

### 5. Run the #764 smoke against prod
```bash
export ATO_API_BASE="https://api.ato.run"
export ATO_SESSION="<operator session token>"
export ATO_RUNNER_ID="<OCI runner id>"
export ATO_CAPSULE_REF="community/local-llm-chat"
export ATO_RUNNER_SSH="oci-linux-test"        # direct --sandbox-absent proof
scripts/smoke-connected-native-inference.sh
```

## Rollback per risky step

| Step | Rollback |
|------|----------|
| ato-api prod deploy | `npx wrangler rollback 8e57dda3-fa55-4e99-b72e-a8f955c81e0d --env production -m "revert"` (prior good version; **code-only** — 0075 is additive/back-compat, no DB rollback needed; secrets unaffected) |
| OCI runner | restore `~/.local/bin/ato.bak.0.5.5` + restart prior `serve`; or revoke the runner in the control plane |
| Store registration | unpublish / delete the `capsule_source_recipes` + `capsules` rows for `community/local-llm-chat` (or unfeature) |

## Capability operational notes (#763)
- `native-inference` advertisement is decided **once at `ato runner serve`
  startup** and cached in a process-lifetime `OnceLock` (`runner_agent.rs:991`).
  **There is no live refresh** — after `ato runner provision` (e.g. adding a GPU),
  you **must restart `ato runner serve`** to advertise the new capability.
- In-session degradation (model cache becomes unwritable mid-session) keeps the
  host advertising native-inference until restart; the on-device
  `ensure_dispatch_supported` guard reads the same cached value (protects against
  control-plane mis-dispatch, not in-session degradation). A periodic re-probe is
  a possible future hardening, deliberately traded for cheapness today.

## Required secrets summary
- **ato-api prod**: already set on the live Worker (no new secret for #149).
- **#764 smoke**: `ATO_SESSION` (operator session), optional `ATO_RUNNER_SSH(_KEY)`.
- **publish**: operator access to `wrangler`/D1 prod (the `publish-submission.mjs` path).

## Preflight checklist (before executing)
- [ ] Decision 1 made (isolate #149 vs promote `main`); if promoting, the 11 bundled PRs reviewed for prod
- [ ] Decision on migration 0075 (apply iff promoting #140's paywall code)
- [ ] OCI nightly build green + `ato doctor native-inference` = ready
- [ ] OCI runner re-enrolled, stale serve stopped, only the new binary serving
- [ ] `local-llm-chat` capsule.toml has a `runtime = "native-inference"` target
- [ ] (recommended) full staging rehearsal GREEN before prod
- [ ] operator session token + OCI SSH available for the smoke

## #754 stays blocked until
1. ato-api #149 in production · 2. a #763-bearing runner · 3. it advertises
native-inference · 4. `local-llm-chat` resolvable · 5. #764 smoke GREEN.

## Final recommendation
**NO-GO** until Decisions 1–3 are made. Then **GO** via the corrected sequence,
with a **staging rehearsal first**. Lowest-risk path: **isolate #149** (Decision B),
**register + smoke on staging**, then promote to prod. Prod deploy + fleet build +
registration each await explicit approval.
