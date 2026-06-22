# Publishing your app to the Ato Store

> Audience: developers (human or AI agent) who have an app in a public GitHub
> repository and want it runnable by anyone through the Ato Store.
> Everything in this document reflects the implementation as of June 2026
> (manifest `schema_version = "0.3"`, store-apply pipeline). Where the design
> docs and the implementation differ, this document describes the
> **implementation**.

> Direction note: the publish entry point is being redesigned as a
> "Capsule Request" pipeline (`ato.run/github.com/<owner>/<repo>` →
> deterministic repo analysis → AI-generated capsule → QA → auto-publish on
> GREEN). See `ato-api: docs/rfcs/draft/capsule-request-pipeline.md`. Until that
> ships, this document describes the live flow — and its verified-TOML path
> remains the fast path afterwards.

This is not a `capsule.toml` reference. It is a procedure for converting an
existing codebase into a Capsule that the Store can distribute and that
actually starts on someone else's machine. The full schema reference lives in
the `ato` repo (`crates/capsule/src/foundation/types/manifest.rs` is the
source of truth); §14 lists pointers.

---

## 1. Start here: the shortest route to the Store

Total time for a simple app: ~15 minutes plus review.

```bash
# 0. In your project root
ato init                  # scaffold a capsule.toml (or write one by hand, §4)

# 1. Make it run locally through Ato — this is the publishability gate
ato run .                 # ato resolves runtime, builds, starts, opens the port
ato validate              # manifest schema check
ato lock                  # pin resolved toolchain/runtimes → capsule.lock.json

# 2. Commit capsule.toml (and capsule.lock.json) to your public GitHub repo
git add capsule.toml capsule.lock.json && git commit -m "Add Ato capsule manifest" && git push

# 3. Submit a Store listing application
#    (web form on ato.run, or the API directly — §9)
curl -X POST https://api.ato.run/v1/store/apply \
  -H "Cookie: better-auth.session_token=$ATO_SESSION_TOKEN" \
  -H "Content-Type: application/json" \
  -d @apply.json          # payload shape in §9

# 4. After review + publish, anyone can run it:
#    Store page  https://ato.run/store/<publisher>/<slug>
#    PWA         app.ato.run → Store tile → Run
#    API         POST /v1/runs  { "source": "<publisher>/<slug>" }
```

Two important reality checks, up front:

- **There is no self-serve `ato publish`-to-Store today.** The `ato publish`
  CLI subcommand exists for registry operations, but Store listings go through
  the **store-apply pipeline**: you submit an application
  (`POST /v1/store/apply`), an operator reviews and approves it, and an
  operator-side publish step makes it live. Status flow:
  `draft → submitted → (needs_changes) → approved → published`.
- **Publishing is the last step of verification, not a one-shot upload.** A
  listing is only considered done when the capsule actually *runs* on the
  verification runners, not merely when it appears in discovery. Design your
  manifest so it starts on a machine that is not yours.

## 2. Core concepts

| Term | Meaning |
|------|---------|
| **Ato** | Runtime that executes projects with no manual setup — it resolves runtimes, builds, sandboxes, and runs from a single manifest. |
| **Capsule** | The execution contract for an app: code (a pinned git commit), runtime, build/run commands, ports, network policy, persisted state. Declared in `capsule.toml`. |
| **Ato Store** | Where users discover, fetch, and run Capsules (`ato.run/store`, the PWA at `app.ato.run`, and the CLI/desktop). |
| **Publisher** | The namespace a Capsule is published under. Community listings use the shared `community` publisher; owner-verified listings use your own handle. |
| **Store ref** | `publisher/slug` (lowercase kebab), e.g. `community/hello-capsule`. This is what `POST /v1/runs` and the CLI accept. URL forms are rejected. |
| **Revision / pinned commit** | A listing pins a 40-hex commit SHA. The Store serves that exact source, not "whatever main is now". |
| **Recipe** | The capsule.toml the Store serves for a listing. It can live in your repo (`origin: repo`) or be uploaded with the submission (`origin: uploaded`). |
| **Launch / Run** | Execution is separate from listing: a run resolves the recipe, gains user consent for its declared permissions, then installs/prepares/builds/executes on a runner (the user's own machine or a Connected Runner). |

The execution pipeline (install → prepare → build → verify → execute) is what
the Store's run-verification exercises. "Publish" means "this contract passed
that pipeline somewhere other than your laptop".

## 3. What makes an app publishable? (checklist)

Submission-time requirements (enforced by the API or by review):

- [ ] **Public GitHub repository** (`github.com/...` only).
- [ ] **A `capsule.toml`** — in the repo (give its path) or uploaded with the
      submission.
- [ ] **`ato run .` starts the app** from a clean clone.
- [ ] **A 40-hex commit SHA** to pin (the submission resolves your ref to one).
- [ ] **Listing metadata**: name (≥2 chars), kebab-case slug, tagline (≥4),
      short description (≥8), maintainer name.
- [ ] **License** you have actually reviewed (SPDX id; OSI/source-available
      that permits self-hosting).
- [ ] **Policy attestations** — all five must be true: `github_public_only`,
      `no_paid_distribution`, `license_reviewed`, `no_malware`,
      `no_secret_exfiltration`.

Manifest-quality requirements (what makes it pass run-verification):

- [ ] The listening **port is declared** (`port = ...` in the target).
      Don't rely on a random or implicit port.
- [ ] Required environment variables are declared (`required_env`), and host
      env passthrough is explicit (`[isolation] allow_env`).
- [ ] **No `.env`, keys, or secrets** in the repo or the manifest.
- [ ] Files the app must persist go through **`[state]`**, not arbitrary host
      paths or `$HOME`.
- [ ] External hosts the app talks to are listed in **`[network] egress_allow`**.
- [ ] `[source].repository` matches the repo's **canonical `Owner/Repo`**
      casing — the client validates the fetched recipe against provenance.
- [ ] README, license file, and a usable icon/banner (see §8 — images are
      hotlinked, so they must be stable live URLs).

The bar is "starts the same way on a stranger's machine", not "works on my
machine".

## 4. capsule.toml tutorial

`schema_version = "0.3"` is the only accepted version. The practical shape is
the **named-target** form below.

### Minimal example (Python web app)

```toml
schema_version = "0.3"
name = "hello-capsule"
version = "1.0.0"
type = "app"
description = "Demo FastAPI app"
default_target = "main"

[source]
repository = "your-github-name/hello-capsule"   # canonical casing!

[targets.main]
runtime = "source"
driver = "python"
runtime_version = "3.11"
run = "python -m uvicorn app.main:app --host 127.0.0.1 --port 8000"
port = 8000
```

### Field tour, in the order you'll need them

1. **Top level** — `schema_version`, `name` (kebab), `version`, `type`
   (`app` | `tool` | `job` | `library` | `inference`), `description`,
   `homepage`, `default_target`, and `[source] repository`.

2. **`[targets.<label>]`** — one per runnable thing.
   - `runtime`: `"source"` (run from source), `"oci"` (container image),
     `"wasm"`, `"web"` (static).
   - `driver` (source runtime): `python`, `node`, `deno`, `static`, `rust`, …
   - `runtime_version`: pin it (`"20"`, `"3.11"`). Don't assume host tools.
   - `run` / `build` / `install` / `prestart`: lifecycle commands.
   - `port`: the port your app listens on. **Always declare it.**
   - `required_env`: env var names the user must provide.
   - `env`: env vars you set.
   - OCI targets: `image` (prefer digest-pinned), `cmd`, `user`,
     `run_once = true` for one-shot jobs (migrations), `depends_on`.

3. **Build lifecycle** — when the repo needs a build before `run`:

   ```toml
   [targets.main]
   runtime = "source"
   driver = "node"
   runtime_version = "20"
   build = "npm install && npm run build"
   run = "npm start"
   port = 3000
   ```

4. **`runtime_tools`** — backend runtime plus a different build toolchain
   (the FastAPI + Vite case):

   ```toml
   [targets.main]
   runtime = "source"
   driver = "python"
   runtime_version = "3.11"
   runtime_tools = { node = "20" }       # Node available for the build step
   build = "cd frontend && npm install && npm run build"
   run = "python -m uvicorn app.main:app --host 127.0.0.1 --port 8000"
   port = 8000
   ```

5. **`[network]`** — egress is allowlist-based. Declare every external host:

   ```toml
   [network]
   egress_allow = ["api.openai.com", "registry.npmjs.org"]
   ```

6. **`[isolation]` and env/secrets** — never bake secrets in. Declare what
   you need and let the user/runner supply it:

   ```toml
   [targets.main]
   required_env = ["OPENAI_API_KEY"]

   [isolation]
   allow_env = ["HF_TOKEN"]     # host env passthrough, explicit only
   ```

7. **`[services]`** — multi-process apps (app + db + worker). Each service
   points at a target; `depends_on` and `readiness_probe` order startup:

   ```toml
   [services.db]
   target = "db"
   readiness_probe = { exec = ["pg_isready", "-U", "app"], port = "5432", timeout_seconds = 90 }

   [services.main]
   target = "app"
   readiness_probe = { http_get = "/", port = "3000", initial_delay_seconds = 10 }
   ```

8. **`[state]`** — anything that must survive restarts (db data, uploads,
   config). Declare the state, then mount it into a service:

   ```toml
   [state.data]
   kind = "filesystem"
   durability = "persistent"
   purpose = "application database"
   attach = "explicit"

   [[services.main.state_bindings]]
   state = "data"
   target = "/var/lib/app/data"
   ```

Other real sections you may need: `[transparency]` (binary allowlist policy:
`strict`/`loose`/`off`), `[build]` (attestation/lifecycle policy), `[ingress]`
(multi-service OCI routing), `metadata` (display name, icon, tags),
`requirements` (platform, disk, VRAM). See §14.

## 5. Recipes by app type

There are **600+ working recipes** in the `ato` repo under
`samples/recipes/<name>/capsule.toml` — find one shaped like your app
and copy it. Reference picks:

| App shape | Recipe to copy | Pattern |
|-----------|----------------|---------|
| Static / simple Node web | `a-dark-room` | `runtime="source"`, `driver="node"`, `run="npm start"` |
| Rust server from source | `atomic-server` | `driver="rust"`, `runtime_version="1.78"` |
| Single OCI container + persistent state | `nocodb` | `runtime="oci"`, `[state]`, readiness probe |
| Multi-service (db + redis + migration + app) | `affine` | `depends_on` chain, `run_once` migration, state bindings |
| Python server app | `hello-capsule` (ato-core/hello-capsule) | source + uvicorn |

For backend + frontend-build combos (FastAPI+Vite, Django+Vite, Rails+esbuild)
the key pieces are `runtime_tools` plus a `build` command — §4 item 4.

## 6. Migrating an existing app to Ato

The job is: **move your README's setup section into `capsule.toml`.** When you
are done, the README's "getting started" should be redundant.

| Existing README says | Goes into |
|----------------------|-----------|
| "install Node 20" | `runtime_version = "20"` (or `runtime_tools`) |
| `pip install -r requirements.txt` | automatic for python targets (declare `runtime_version`; `dependencies` file hint if non-standard) |
| `npm install && npm run build` | `build = "..."` |
| `cp .env.example .env` | `required_env = [...]` per target |
| `DATABASE_URL=...` | service `env` (internal) or `required_env` (user-supplied) |
| "open localhost:3000" | `port = 3000` |
| `mkdir uploads` | `[state.uploads]` + `state_bindings` |
| "uses the OpenAI API" | `[network] egress_allow` + `required_env` |
| `docker compose up` | `[services]` with one OCI target per compose service |

Anti-patterns that will fail review or run-verification:

- Committing `.env` or any secret.
- Writing to `$HOME` or absolute host paths instead of `[state]`.
- Assuming a random/implicit port, or listening on interfaces other than the
  declared one.
- Depending on host-installed Node/Python/Postgres ("works because my laptop
  has it").
- Setup steps that exist only in the README and not in the manifest.

## 7. Permissions, security, and user consent

Users see and consent to what your manifest declares before the app runs.
Current enforcement status — be honest with yourself about both columns:

| Surface | Declared in | Enforced today |
|---------|------------|----------------|
| Network egress | `[network] egress_allow` / `egress_id_allow` | Allowlist routed through the egress proxy on sandboxed runners; declare *everything* you contact |
| Env / secrets | `required_env`, `[isolation] allow_env` | Required env gates launch; host passthrough only via `allow_env` |
| Persistent state | `[state]` + `state_bindings` | Mounted paths only; writes elsewhere are sandbox-scoped/ephemeral |
| Binary transparency | `[transparency]` | `strict` by default; pre-built binaries need `allowed_binaries` globs |
| Source identity | pinned commit + `[source].repository` | Client checks fetched recipe against provenance |
| GPU / heavy resources | `requirements` (vram/disk) | Surfaced to placement; declare honestly |

Pre-1.0 caveat: not every control is fully enforced on every platform yet.
The rule for publishers is: **declare everything regardless** — declarations
are the consent surface users see, and enforcement only tightens over time.

## 8. Store listing metadata

Separate from `capsule.toml`; carried in the submission's `listing` block:

- `name`, `slug` (kebab, unique per publisher), `tagline`,
  `short_description`, `full_description`, `category`, `tags`
- `maintainer_name`, `license`
- `icon_url`, `banner_url` — **hotlinked as-is** by the Store today, so they
  must be stable, live image URLs. Reliable fallbacks that never 404:
  - icon: `https://github.com/<owner>.png?size=256`
  - banner: `https://opengraph.githubassets.com/1/<owner>/<repo>`
  - For uploaded media, only Ato-hosted asset URLs are accepted
    (`POST /v1/uploads/store-apply-image` mirrors an image for you).

The listing also surfaces, from the manifest, *what the app needs*: network
destinations, required env, persisted state, platforms. Users decide with
that; keep it truthful.

## 9. Publish flow (the actual pipeline)

```
draft → submitted → (needs_changes →) approved → published → run-verified GREEN
        you                review              operator        verification
```

1. **Sign in** at ato.run (Better-Auth session; API calls authenticate with
   the session cookie `better-auth.session_token`).
2. **Submit** — `POST /v1/store/apply` with:

   ```jsonc
   {
     "intent": "submitted",          // or "draft" to save without submitting
     "source": {
       "github_url": "https://github.com/Owner/Repo",
       "owner": "Owner", "repo": "Repo",          // canonical casing
       "ref_input": "main",
       "resolved_commit_sha": "<40-hex>",
       "capsule_toml_path": "capsule.toml"        // in-repo recipe…
       // …or "capsule_toml_content": "<toml>"    // uploaded recipe
     },
     "listing": { "name", "slug", "tagline", "short_description",
                  "full_description", "category", "tags",
                  "icon_url", "banner_url", "maintainer_name", "license" },
     "policy": { "github_public_only": true, "no_paid_distribution": true,
                 "license_reviewed": true, "no_malware": true,
                 "no_secret_exfiltration": true }
   }
   ```

   Returns `{ submission_id, status, checks }`. Manage your submissions with
   `GET/PUT/DELETE /v1/store/apply[/:id]` and `POST /v1/store/apply/:id/withdraw`.
3. **Review** — an operator checks the license, manifest, and listing.
   `needs_changes` comes back with `review_notes`; fix and resubmit (PUT).
4. **Publish** — on approval, the operator publish step creates the capsule
   under the publisher namespace, pins your commit, snapshots the source
   archive, and makes the recipe discoverable. Slug collisions are resolved by
   suffixing (`slug-repo`, `slug-owner-repo`, `slug-2`…), and forks that
   shadow an upstream app don't get the owner-verified path.
5. **Run-verification** — the capsule is run on the Connected Runner fleet
   (macOS/Linux/Windows). A listing is done at **GREEN** (it runs), not at
   "published" (it resolves). Failures come back as recipe fixes to make.
6. **Live** — Store page `https://ato.run/store/<publisher>/<slug>`; runnable
   via the PWA, desktop, CLI, or `POST /v1/runs {"source":"<publisher>/<slug>"}`.

## 10. Review policy: why submissions get rejected

Practical guide to passing review, roughly in order of frequency:

1. **It doesn't start** — `ato run .` fails from a clean clone. Test this first.
2. **Manifest gaps** — missing port, undeclared env, missing egress hosts.
3. **Secrets in the repo** — instant reject; rotate the secret too.
4. **License missing/unclear**, or distribution terms that forbid listing.
5. **Fork/ownership confusion** — submitting a fork as if it were the upstream
   app without controlling the upstream account.
6. **Misleading listing** — name/screenshots/description that don't match what
   runs.
7. **Hostile behavior** — malware-like patterns, secret exfiltration, host
   pollution outside the sandbox, destructive writes to user data.
8. **Excessive permissions** — egress or env far beyond what the app's purpose
   explains. Narrow it or justify it in the description.

## 11. Updates, versioning, compatibility

- Listings pin a commit; **an update is a new submission** with a new
  `resolved_commit_sha` (and normally a bumped `version`). Re-publishing
  supersedes the previous recipe for the same source.
- Use semver in `version`: patch = fixes, minor = features, major = breaking.
- **Adding permissions** (new egress hosts, new env, new state) changes the
  consent surface — expect users to re-consent. Don't widen silently.
- **Code and user data are separate.** `[state]` declared storage survives app
  updates; if you change your data layout, ship a migration (a `run_once`
  migration service, as in the `affine` recipe, is the standard pattern).
- Rollback = the previous pinned revision still exists; users can keep running
  it, and `ato rollback` restores a previous installed revision locally.

## 12. CI / automation

What you can automate today:

- **Validate on every PR**: `ato validate` (+ `ato lock` and diff the lockfile).
- **Smoke test**: `ato run .` with a readiness check in CI (Linux runners work).
- **Submission via API**: `POST /v1/store/apply` is plain JSON + session auth,
  so a release workflow can submit the new commit SHA automatically.

Not yet available (don't build against these): self-serve `ato publish --ci`
to the Store, OIDC/keyless publish, draft/preview listings in CI. The
approve/publish steps remain operator-side.

## 13. Troubleshooting

| Symptom | Likely cause / fix |
|---------|--------------------|
| `ato validate` fails | Wrong `schema_version` (only "0.3"), unknown field, non-kebab `name` |
| Runtime not found | Missing/loose `runtime_version`; run `ato lock` and commit the lockfile |
| Build failed | `build` command assumes host tools → use `runtime_tools`; build-time network blocked → declare it |
| Starts but page 404s/blank | Wrong `port`; app listening on a different interface than declared; missing readiness probe so it's checked too early |
| Missing env / secret | Add to `required_env` (user-supplied) or `[isolation] allow_env` (host passthrough) |
| Network blocked | Host not in `[network] egress_allow` — check the run log for the denied domain |
| Permission denied writing files | Writing outside `[state]` mounts; add a state + binding (set `owner`/`mode` if the container user needs it) |
| Works locally, fails verification | Host-installed dependency assumed; un-pinned versions; OS-specific path. Categories from verification: `toml` (fix manifest), `app` (often really missing env/egress), `ato` (platform gap — file an issue, don't loop) |
| OCI image fails on a runner | Architecture mismatch — pin a multi-arch image or set `allow_emulation` |
| Where are the logs? | `ato logs`, `ato receipts` (execution receipts), `ato ps` locally; verification failures return `category` + `why` + `log_tail` |

## 14. Full reference pointers

| Topic | Source of truth |
|-------|-----------------|
| `capsule.toml` schema | `crates/capsule/src/foundation/types/manifest.rs` (+ `manifest_validation.rs`) |
| Lockfile (`capsule.lock.json`) | `crates/capsule/src/contract/lockfile.rs` |
| CLI commands | `ato --help`; clap definitions in `crates/ato-cli/.../root.rs` (`run`, `validate`, `lock`, `encap`, `install`, `launch`, `rollback`, `logs`, `receipts`, `secrets`, …) |
| Store apply API | `ato-api: src/routes/store_apply.ts` |
| Discovery / capsule API | `ato-api: src/routes/capsule_tomls.ts`, `capsules.ts`, `runs.ts` |
| Slug/fork/publish policy | `ato-api: docs/store-apply-decisions.md`, `store-ownership-fork-policy.md` |
| Operator review runbook | `ato-api: docs/runbooks/store-apply-review.md` |
| 600+ example recipes | `samples/recipes/<name>/capsule.toml` |
| Identity rules | Store refs are `publisher/slug` with exact pinned commits; GitHub-direct runs use 40-hex commit SHAs as point-in-time identity |

---

### Priority reading order

1. §1 (shortest route) → 2. §4 (capsule.toml tutorial) → 3. §6 (migrating an
existing app) → 4. §3 (submission checklist) → 5. §5 (recipes). Everything
else is consultable when you hit it.
