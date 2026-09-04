# Step 10 — Process Personal App, staging acceptance

A FastAPI + SQLite folder becomes an ordinary Personal App through the ordinary
Add App flow, and keeps its data across sleeping and waking.

Driven by hand in Chrome. Everything below is the part that a test cannot
assert: whether a person, looking at the screen, can tell that this App is any
different from a page of HTML — and whether their data is still there.

Companion runbooks: `p4-stable-instance-host.md` (the stable URL, cold wake and
seal that this builds on) and `b1-formation-staging-acceptance.md` (Formation's
own staging lane).

Read `docs/rfcs/…` Capsule Process Model v0.2 first if the words below are new:
a Capsule's identity IS its Contract `K`, and a Derivation is one executable
route to satisfying it. `capsule.toml` and Presets are two authoring frontends
for the same Contract + Derivation pipeline.

---

## 0. What is being tested, and what is not

**Tested here.** Add App with a folder whose `capsule.toml` declares a process
route; the stable URL serving that process on its first request; data surviving
sleep and wake; isolation between two Apps; the Storage number; Remove App.

Also worth reading off the Formation result while you are in the API: every
successful build now records `contract_ref`, `derivation_ref` and a
`contract_verification` block naming, per observation, whether it was satisfied
at formation time or deferred to the run-time readiness gate.

**Not tested here, already pinned by tests.** Cold-wake single flight
(`ato-api src/tests/p4b-single-flight-wake.test.ts`), the endpoint's lifetime
(`p4a-instance-endpoint.test.ts`), the authoring pipeline and both digests
(`ato lib/formation/tests/authoring_v1.rs`), the state-slot refusals and the
first-request lane choice (`ato-api src/tests/personal-apps-process-v0.test.ts`).

**Explicitly out of scope for Step 10.** Update S1 → S2 for a Process App.
The code path exists and carries the same state-slot contract, but it is not a
merge gate and is not driven below.

---

## 1. Preconditions

| | |
|---|---|
| Runner | staging `ubuntu-sugamo`, Connected Realization Worker, online and holding a `runtime_launch` lease slot |
| Ingress | the incumbent's slot, restored. **This runbook does not take `s2-rstg002.ato.run`.** |
| API | `staging.api.ato.run` at the Step 10 merge |
| PWA | `stg-app.ato.run` at the Step 10 merge |
| Account | one staging account, its user id to hand |

One staging var, on the API worker:

```
PERSONAL_APPS_V0_ENABLED = "true"                     # already set
```

There is **no process allowlist**. `requested_outputs` is an admission
contract — what Formation MAY return — not a lane selector, so Add App asks for
`["static_web", "process_workspace"]` for everybody. What is actually produced
comes from executing the Derivation the author wrote or the Preset synthesized;
a folder of HTML still forms a static surface because that is what its
Derivation does, not because the control plane decided so in advance.

## 2. The fixture

`samples/fastapi-sqlite-personal/` in the `ato` repo — three files:

- `main.py` — a note keeper. `POST /api/notes` writes to SQLite, `GET /api/notes`
  reads it back, `/` is a page that does both, `/health` opens the database.
- `requirements.txt` — `fastapi`, `uvicorn`, pinned.
- `capsule.toml` — a **Contract** and one **Derivation** that proposes a way to
  reach it. `[[contract.require]]` is what this Capsule IS: the observable
  conditions that decide whether a future computation counts as the same
  resumable point. Everything else is one route there.

```toml
schema = "ato.capsule/1"

[[derive.step]]
id = "app"
use = "ato.process@1"
op = "serve"
argv = ["/opt/ato/toolchains/python/3.12.7/bin/python3", "-m", "uvicorn",
        "main:app", "--host", "0.0.0.0", "--port", "8000"]

[[port]]
id = "app.http"
use = "ato.http@1"
from = "app"
guest_port = 8000

[[state]]
id = "app_data"
use = "ato.state.filesystem@1"
mount = "/data"

[[contract.require]]
id = "app-responds"
use = "ato.contract.http@1"
port = "app.http"
path = "/health"
[contract.require.expect]
status = 200

[[contract.require]]
id = "source-identity"
use = "ato.contract.workspace@1"
input = "workspace"
[contract.require.expect]
digest = "capture"
```

The full file is in the repo. Note what the Contract deliberately does NOT
observe: the response body. This app's notes change on every save, so observing
them would mint a new Capsule identity each time somebody typed.

Copy the folder to the desktop. It is uploaded as a folder, as it stands, with
no packaging step.

---

## 3. Add the App

1. Sign in at `stg-app.ato.run`. Go to **My Apps → Add App**.
2. Drag `fastapi-sqlite-personal/` onto the drop target.

**Assert while it runs:**

- Nothing asked what kind of app this is. No mode selector, no "server /
  static" choice, no advanced section.
- The progress line keeps changing and never goes blank. This build resolves
  `fastapi` and `uvicorn` from the network, so expect **one to three minutes**,
  not the seconds a static site takes.
- **Cancel** stays on screen the whole time.
- No line names a Run, a Runner, a build system, a VM or a container.

**Assert when it finishes:**

- The **Ready** screen, identical to a static App's: "Your app is ready", the
  `https://cinst-….stg-app.ato.run/` URL, "permanent link", Open App, Copy link.
- No Start / Launch / Wake button. There is nothing for a person to start.

*If it fails:* the failure line is written for the uploader.

- `derive.step.app.argv ... never inferred` — the folder's `capsule.toml` did
  reach Formation and is missing something. Fix the document; it is never
  replaced by a guess.
- `unsupported_capsule_schema` — the document declares neither
  `schema = "ato.capsule/1"` nor a store `schema_version`. Refused on purpose.
- `did not satisfy this app's contract` — the build ran and the candidate did
  not satisfy `K`. Formation succeeds on `C' ⊨ K`, not on "the build finished",
  so this is the system working.

## 4. First open — the cold wake, on the App's own URL

Copy the URL. Open it in a **new tab**.

- The tab may sit for 10–60 s. That is the wake: no Run existed, the schema
  said "process", and the request is being held rather than answered wrongly.
- **Assert the address bar never changes.** No redirect to a Run-specific host.
  This is the single most important assertion in the runbook — the stable URL
  is the product.
- The notes page renders.
- If it 503/504s with `instance_wake_failed`, reload once. Persisting means no
  Runner slot; check `ubuntu-sugamo` before blaming the lane.

**Cold-wake single flight.** Close the tab. Wait for the App to sleep
(§6), then open **two tabs at the same moment** on the same URL. Both must
render. Then confirm in the API that exactly **one** Run was started for that
wake — two Runs racing for one state slot is the failure this exists to
prevent, and it is invisible from the browser.

## 5. Write something

In the page, add three notes: `first`, `second`, `third`. Each appears in the
list with a timestamp.

Reload. All three are still there — that only proves the process is still up.

## 6. Sleep, then wake — the actual claim

1. Close every tab on the App.
2. Wait out the idle timeout (see `p4-stable-instance-host.md` for the current
   value; do not shorten it for the run).
3. Confirm the App is asleep: the Run has ended and its state slot has been
   **sealed** — a new head revision exists for `app_data`. A slot still holding
   a writer means the seal did not happen, and everything after this is
   meaningless.
4. Open the URL again. It cold-wakes exactly as in §4.
5. **Assert: `first`, `second`, `third` are all there, with their original
   timestamps.**

Then add a fourth note, `fourth`, and repeat 1–4 once. All four survive. One
round trip can pass by accident — a workspace that was never recycled looks
identical to a restore. Two cannot.

## 7. Isolation

Add the fixture a **second time**, as a separate App (same folder, new Add).

- Two tiles in My Apps, two different `cinst-…` URLs.
- Open the second. Its list is **empty** — the first App's notes are not there.
- Write `other` into the second. Reload the first: still `first…fourth`, no
  `other`.

A shared slot would show up here and nowhere else.

## 8. My Data

Open **App Info** for the first App.

- Layout is the same as a static App's: the same panels in the same order.
- **Stored: "Saved as files".**
- The sentence "This app keeps its data in files. Previewing those is not
  supported yet." — Step 4's decision, and deliberately not a preview.
- **Size** is non-zero and roughly the SQLite file's size.
- It does **not** say "Nothing saved yet". A 40 KB database described as empty
  is the one wrong statement that could get somebody to delete their data.
- Nothing on the screen says Run, Runner, Process, VM or ComputeSchema.

## 9. Storage

**Settings → Billing → Storage.**

- **One** meter, not two. The filesystem bytes are inside the same "X of 100.0
  MB used" number that browser state uses.
- The number counts the **current head only**, not the revision history behind
  it. Write another twenty notes across two more sleep/wake cycles and the
  number tracks the database's size, not the number of saves.
- Nothing names a backend, a revision or a lane.

## 10. Remove App

1. App Info → **Remove App**.
2. The confirmation is the same one a static App gets, and it names what is
   about to go — including the files.
3. Confirm.
4. The tile is gone; opening the old URL gives **410**, not 404 and not
   somebody else's App.
5. **Storage drops immediately** — at the moment of the removal, not when a
   reclaimer runs. Re-check §9's number.
6. Remove the second App the same way and confirm Storage returns to where it
   started.

---

## 11. Afterwards

- Report which branch is on `stg-app.ato.run`.

## 12. What cannot be checked without the Runner

Everything in §4–§10 needs a real `ubuntu-sugamo` Run. Nothing below the
Formation boundary was executed while writing this: the Python toolchain
provisioning, the dependency install under `dependency_resolution`, the mount
of `app_data` at `/data`, the readiness probe against `/health`, the seal on
sleep and the restore on wake are all pinned by tests at their own boundaries
and by P4's own acceptance, and by nothing end-to-end for THIS fixture until
this runbook is driven.
