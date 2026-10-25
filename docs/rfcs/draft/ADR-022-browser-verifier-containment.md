# ADR-022 — Browser Verifier containment

**Status**: proposed (amended 2026-09-23: the browser runs in a sibling
sandbox started by the worker, not a nested one — see "Amendment")
**Context**: The last open trust boundary of Browser Verification v0
(ADR-020) and the Runtime Network (ADR-021): before a benchmark runs many
unknown applications, the process that renders them must not see the host.

## The question

A browser Contract is verified by a Node helper that drives Chrome at a
realized, untrusted application. The network boundary exists: the browser can
reach only the candidate's origin (ADR-020's guard proxy). But the helper and
Chrome ran on the host filesystem as the worker's user, and Chrome inherited
the helper's environment — including the judge and agent model keys (Stagehand
launches Chrome through chrome-launcher, whose default environment is
`process.env`). A browser compromised by the page it renders would hold the
keys and the host.

## The decision

```text
worker (host)
  └─ bwrap  verifier sandbox        --unshare-all --share-net --die-with-parent --new-session
       /verifier        ro   helper source + node_modules (+ bin/chrome-contained)
       /runtime/node    ro   the Node install
       /runtime/chrome  ro   the Chrome directory
       /scratch         rw   per-verification: HOME, TMPDIR, browser profile
       /tmp /dev/shm    tmpfs
       /usr /bin /lib …, /etc/{resolv.conf,hosts,nsswitch.conf,ssl,ca-certificates,pki,fonts}  ro
       nothing else: no home, no repository, no credentials, no token or key files
     └─ node helper    keys on fd 3, none in its environment
          └─ /verifier/bin/chrome-contained
               └─ env -i … bwrap --unshare-pid   Chrome: empty environment, own PID namespace
```

- **Verification infrastructure, not a Derivation.** The verifier is never
  lowered as a candidate workload (`RuntimeLaunchSpecV1`) and does not reuse
  the build sandbox's policy. It reuses the primitives: bubblewrap, its
  detection, the system path lists, the credential-path list. The spec is
  `BrowserVerifierSandboxSpec` (`apps/formation-worker/src/browser_sandbox.rs`).
- **Host paths are resolved once and never cross.** The helper root, the Node
  binary (its install root is bound, `<root>/bin/node`) and the Chrome binary
  (its directory) are canonicalized on the host; a bind that is the filesystem
  root, the home directory or anything above it, or that overlaps a
  credential directory or `~/.config` is refused. Inside, only fixed guest
  paths exist; the helper is told `/scratch`, and receipts name the verifier
  `contained-node-helper`, never a host path.
- **Keys never in an environment.** The worker writes `{JEV_API_KEY,
  DEEPSEEK_API_KEY}` to a close-on-exec pipe and hands its read end to the
  helper as fd 3 (`ATO_VERIFIER_SECRETS_FD`). The helper reads and closes it
  and removes any key from `process.env` before anything is launched, so its
  own `/proc/<pid>/environ` carries no key and neither does anything it
  starts. The key file the worker reads (`~/.config/ato/formation-browser.env`)
  is never bound. Failure messages are scrubbed of key values; a receipt that
  contains one is refused (ADR-020).
- **The browser is kept apart from the helper.** Chrome starts through
  `bin/chrome-contained`: `env -i` with only `HOME`, `TMPDIR`, `PATH`, `LANG`,
  then a nested bubblewrap with a new PID namespace. The browser sees the
  same (allowlisted) filesystem but not the helper's process, so
  `/proc/<helper>/environ` and `/proc/<helper>/mem` do not exist for it; the
  keys are in no environment in the first place. If bubblewrap cannot nest,
  the browser does not start.
- **Network unchanged.** The sandbox shares the host network: the helper must
  reach its judge and agent. The browser's traffic stays confined to the
  candidate's `host:port` by the guard proxy (ADR-020), measured again here.
- **Fail closed.** A verifier is usable only when its sandbox preflight
  succeeds — bubblewrap works, and from inside, Node runs and finds the
  browser, the launcher and bubblewrap while the host's home is absent. A
  Runtime advertises `runtime.browser = true` and
  `verifier.browser.containment = bwrap` only then; a Runtime Network worker
  never runs an uncontained verifier, and the coordinator hard-filters a
  browser Contract away from any Runtime without that fact
  (`verifier_containment_unavailable`). Local admission refuses with
  `browser_verifier_containment_unavailable` before anything runs.
- **Development escape hatch, explicit.** `ato form --verify-browser
  --allow-uncontained-browser-verifier` runs the helper on the host where no
  sandbox exists (macOS). It is not accepted by `ato runtime-network serve`,
  never advertised, and every receipt says `containment: none`,
  `filesystem: host`. Keys still travel on fd 3.
- **Cleanup by namespace.** Killing the sandbox's bubblewrap ends its PID
  namespace — helper, guard proxy, browser and the browser's nested namespace
  together — on normal exit, timeout, crash or kill; `--die-with-parent` ends
  it if the worker dies. The scratch directory is removed after each
  verification; one left by a killed worker is swept by the next verification
  (its `.owner` process is gone). The earlier `pkill -f <scratch>` remains as
  defence in depth for the uncontained mode.
- **Evidence.** The receipt carries `containment {containment: "bwrap",
  filesystem: "allowlisted", network:
  "shared-host-net+exact-origin-browser-guard", browser_process:
  "separate-pid-namespace+empty-environment", secrets: "fd"}` — set by the
  worker that launched the sandbox, not claimed by the helper. A
  VerifiedRoute's capability profile records
  `verifier.browser.containment = bwrap`.

## Consequences

- A Runtime needs Linux, bubblewrap with nested user namespaces, a Node
  install and a Chrome binary named explicitly (`--browser-chrome` /
  `ATO_BROWSER_CHROME_PATH`) to offer browser verification.
- macOS Runtimes advertise `runtime.browser = false`.
- The sandbox shares the host network namespace; a helper compromised through
  its dependencies could reach the network (not the filesystem). Narrowing
  the helper's egress to the judge and agent endpoints is a separate step.
- Chrome's own sandbox runs inside ours where the host allows it; renderer
  processes observed from the host were non-dumpable (their `environ`
  unreadable even to the same user).

## Amendment (2026-09-23): a sibling browser sandbox, not a nested one

The first implementation started Chrome inside the helper's sandbox through a
nested `bwrap --unshare-pid`. That needs a user namespace inside a user
namespace. On hosts with AppArmor's `apparmor_restrict_unprivileged_userns = 1`
— the default on Ubuntu 24.04 and later — bubblewrap itself may create one,
but a process it starts may not, so the nested launch failed. Worse, the
preflight checked only that `/usr/bin/bwrap` existed, so such a host
advertised `runtime.browser = true` and then failed every browser
verification (`ECONNREFUSED` to a browser that never started). Found on an
Ubuntu 26.04 x86_64 Runtime before the P0 benchmark; the ARM64 host had the
restriction off.

Now:

```text
worker (host)
  ├─ bwrap helper sandbox   /verifier ro, /runtime/node ro, /scratch rw; keys on fd 3
  │    └─ bin/chrome-contained.cjs ── /scratch/.browser-launcher.sock ──┐
  └─ bwrap browser sandbox  /runtime/chrome ro, /scratch rw; empty env ◀┘ (per request)
```

- The helper sandbox no longer contains Chrome at all. Stagehand starts
  `bin/chrome-contained.cjs`, a client that sends only Chrome's arguments to
  the worker's launcher (`BrowserLauncher`) and then stands in for the
  browser: it exits with the browser's code, and when it is killed the worker
  kills the browser. The worker chooses the executable and the sandbox; at
  most four browsers per verification; all are killed when the verification
  ends.
- The browser sandbox is a sibling of the helper's, created by the worker, so
  no nesting is needed: its own PID namespace (the helper does not exist in
  it), an empty environment plus `HOME`, `TMPDIR`, `PATH`, `LANG`, the Chrome
  directory read-only and the same scratch for the profile.
- The preflight starts both sandboxes: the helper's, and Chrome headless in
  the browser's until it opens its DevTools endpoint — first with Chrome's own
  sandbox, then with `--no-sandbox` (still inside the browser sandbox). A host
  where neither starts is not a browser Runtime. The mode is recorded:
  `browser_process = "separate-sandbox+empty-environment+chrome-sandbox"` or
  `…+chrome-no-sandbox`.
