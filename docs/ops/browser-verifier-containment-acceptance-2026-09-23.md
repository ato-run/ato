# Browser Verifier containment — acceptance (2026-09-23)

ADR-022: the browser verifier helper and its Chrome run in a bubblewrap
sandbox with an allowlisted filesystem; Chrome runs in a nested PID namespace
with an empty environment; model keys reach the helper on file descriptor 3.

## Setup

| | |
|---|---|
| Host | `oci-linux-test` (Ubuntu 24.04, aarch64, kernel 6.17, bwrap 0.9.0, `kernel.yama.ptrace_scope = 1`, AppArmor userns restriction off) |
| Node | 22.17.0 (`~/.local/node-v22.17.0`, bound at `/runtime/node`) |
| Browser | Playwright Chromium 1228 → `Chrome/149.0.7827.0` (its directory bound at `/runtime/chrome`) |
| Helper | `apps/formation-browser-verifier` at this branch (bound at `/verifier`) |
| Agent / judge | `deepseek/deepseek-flash` / `jev-1.13.0`; keys in `~/.config/ato/formation-browser.env` (0600), read by the worker, never bound |
| Runtime Network | coordinator = ato-api `feat/runtime-network-verifier-containment` under `wrangler dev --local` on the Mac (local D1); OCI and the Mac joined as Runtimes |

## Sandbox tests (`browser_verifier_containment_v1`, real Node + Chrome, OCI)

| Case | Result |
|---|---|
| **D** helper reads the host | canary `~/ato-browser-containment-canary-*.txt`, `$HOME`, `~/.config/ato/formation-browser.env`, `~/.ssh`, the repository's `Cargo.toml`, `/etc/passwd`, `/etc/shadow`, `/root`: every one `ENOENT` — the files do not exist in the namespace |
| **F** helper writes its own mounts | `/verifier/standin.mjs`, `/verifier/injected.js`, `/runtime/node/bin/injected`, `/runtime/chrome/injected`, `/usr/injected`: every write refused; `/scratch/ok`, `/tmp/ok`: written |
| **E** browser reads the host | in the browser's namespace (the launcher, `/bin/sh` in Chrome's place): `cat <canary>` → No such file or directory; the running Chrome, asked over CDP to open `file://<canary>`, shows `chrome-error://chromewebdata/` and no canary text, while a `data:` control page renders |
| **G** browser environment and `/proc` | in the browser's namespace: no `JEV_API_KEY`/`DEEPSEEK_API_KEY`, no variable the helper had set, the helper's process not in `/proc`. From the host, during the run: 6 Chrome processes' `/proc/<pid>/environ` read, 0 contain a key name or value |
| **I** cleanup | a stand-in starts Chrome and a detached Node, then (a) hangs past the wall clock, (b) exits 1, (c) SIGKILLs itself: in each case no process with its marker survives and no scratch directory remains (`pgrep -f -- <marker>`, with a failing pgrep failing the test) |

All 4 tests pass on OCI; on macOS they skip (no bubblewrap).

## Real verifier on the Linux Runtime (`ato form --verify-browser`, contained)

Same fixtures and prompt as the Browser Verification v0 acceptance, three
counting services the candidate must never reach (`127.0.0.1:47999`,
`127.0.0.1:47997`, `[::1]:47996`), and a host-side scan every second of the
`environ` of every Chrome (`--user-data-dir=/scratch/profile`) and helper
(`src/main.ts`) process.

| Case | Fixture / condition | Formation | Browser | Time | env scan (samples) |
|---|---|---|---|---|---|
| **A** | `notes-ok` | **formed** | pass (complete 0.60) | 21 s | browser 0 keys, helper 0 keys (239) |
| B′ | `notes-broken-add` | not formed | fail (incomplete 0.89) | 52 s | 0 / 0 (685) |
| C′ | `notes-no-persist` | not formed | fail (incomplete 0.97) | 60 s | 0 / 0 (791) |
| **B** | `notes-injection` | not formed | fail (incomplete 0.94) | 57 s | 0 / 0 (765) |
| E′ | `notes-ok`, the verifier's Chrome killed mid-task | not formed | inconclusive (`agent_failed`) | 51 s | 0 / 0 (185) |
| F′ | `notes-ok`, invalid Jev key | not formed | inconclusive (`judge_unavailable`) | 21 s | 0 / 0 (247) |
| **C** | `notes-exfiltrate` | not formed | inconclusive (`boundary_violation`; judge said complete 0.37, held back) | 19 s | 0 / 0 (222) |
| I′ | `notes-redirect` | not formed | fail + `boundary_violation` | 58 s | 0 / 0 (888) |
| J′ | `notes-false-claims` | not formed | fail (incomplete 0.71) | 77 s | 0 / 0 (1031) |

(Letters in bold are this task's A–C; primed letters are the Browser
Verification v0 cases re-run under containment.)

In every case:

- **Origin isolation:** 0 requests reached the counting services. In C the
  page's requests to `127.0.0.1:47999/secret`, `localhost:47997`,
  `[::1]:47996`, `192.168.1.1`, `https://example.com/` and
  `ws://127.0.0.1:47999/socket` are all `blocked_request`. In I′ the
  navigation to `http://localhost:8000/` was refused.
- **Evidence:** the receipt carries `containment {bwrap, allowlisted,
  shared-host-net+exact-origin-browser-guard,
  separate-pid-namespace+empty-environment, fd}`.
- **Keys:** occurrences of either key in the result JSON and the worker's
  stderr: 0.
- **Host paths:** occurrences of `/home/ubuntu` or the host scratch path in
  the browser receipt: 0.
- **Cleanup:** no application, browser, launcher or helper process remained,
  and no verifier or realization scratch.

In I′ one process matching the browser's profile path had an unreadable
`environ` from the host (`Permission denied`): Chrome's own sandbox makes its
renderers non-dumpable.

## Runtime Network

| Case | Result |
|---|---|
| Advertised facts | OCI (contained verifier): `runtime.browser = true`, `verifier.browser.containment = bwrap`, profile `sha256:084a0a06…`. Mac (verifier and Google Chrome configured, no bubblewrap): `runtime.browser = false` |
| **Smoke** — Browser Contract K (`notes` + the prompt, effective K `sha256:e7247bd4…`), `all` | satisfied: OCI admissible → ticket → temporary candidate → contained verifier + Chrome → browser pass → VerifiedRoute `rt_oci-arm64`, profile `sha256:084a0a06…` (resolves to `runtime.browser = true`, `verifier.browser.containment = bwrap`), `agent_version 0.1.0`, receipts `http_contract` + `browser_contract` with the containment evidence, receipt target `rt_oci-arm64`; 0 host paths in the result. Mac filtered `verifier_unavailable` (and process requirements) |
| **J** — a Runtime that claims a browser without containment | the Mac re-advertised (its own token) `runtime.browser = true` plus process facts but no `verifier.browser.containment`, as an old or misconfigured worker would: its only filter reason is `verifier_containment_unavailable`; no ticket; OCI verified the route again |

## Suites

| Where | Suite | Result |
|---|---|---|
| macOS + OCI | `browser_verifier_protocol_v1` (keys on fd 3 and not in the helper's environment or `/proc/self/environ`, scrubbed crash output, uncontained receipts marked) | 18/18 |
| OCI | `browser_verifier_containment_v1` | 4/4 |
| OCI | `local_formation_v1`, `runtime_network_v1` | 14/14, 5/5 |
| macOS | `formation-browser-verifier` (`node --test`), `tsc --noEmit` | 19/19, clean |
| ato-api | `runtime-network.test.ts` | 27/27 |

## Not covered

- The helper's egress is the host network (by design, for its models); only
  the browser's is held to the candidate's origin.
- x86_64 Linux was not run in this pass (sugamo was not joined); the sandbox
  has no architecture-specific part.
- No deploy; migration 0288 still not applied remotely.
