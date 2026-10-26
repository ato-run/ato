# Native application observation Adapter — 2026-10-24

Depends on portable Native admission #1514 and bounded RFB capture #1515.
The common sidecar now accepts the selected D's shared typed native catalog and
an owned private endpoint. Expected K answers are never passed to the Adapter.

It performs every declared bounded operation once, freezes measured results,
and publishes a separate loopback HTTP observation port only when all finish.
GET retrieval does not replay input. Terminal results are actual bounded bytes;
pixel results use the shared 72-byte measured-frame fingerprint encoder.
Existing HTTP observation size limits are unchanged. Only existing Rust
`verify_runtime` decides K; sidecar output always says `contract_verified=false`.

## Assembly boundary

The common `ato-native-surface-gateway` binary is owned by the application
crate at `apps/native-surface-gateway`. It combines Formation's typed catalog
with netd's reusable transport; netd itself has no Formation dependency.
The initial CI correctly rejected service-to-lib coupling. The architecture
rule is unchanged, and the corrected workspace passes `arch-check` (44 crates).
The binary name, configuration, transport and observation behavior are unchanged.

## Verification

- After moving assembly: macOS application all targets (5 probe tests, 1 parser
  test, 3 actual sidecar process tests), application/netd all-target clippy,
  locked dependency metadata and workspace format checks passed.

- Linux netd all targets: 118 tests passed before the final additional
  queued-completion regression; the final native module's five tests passed,
  including that new regression (119 distinct netd tests in total).
- All-target clippy with `--locked -- -D warnings`: passed.
- Actual sidecar binary tests: configuration refusal before native connection,
  cancellation of an in-flight 30-second action within two seconds on owner
  EOF, single action despite repeated GETs, and teardown of both listeners and
  pending clients. Scoped key/idle/Origin/Run validation remains fail-closed.
- The TCP result is fed into the same Rust HTTP K verifier: measured answer
  succeeds; readiness text, wrong Port and missing observation fail. This is a
  socket fixture test, not a Hosted receipt claim.
- Echo-only, overflow, unexpected control/exit, trailing output, timeout and a
  later failed operation never publish an observation port. A completion
  already queued before input does not count. Catalog line-profile validation
  also refuses normal CR/LF/ECHOCTL echo transforms.

Real CPU Linux acceptance ran inside private bwrap mount/PID/network namespaces
with read-only binaries and only a fresh owned work directory writable. Both
real Agent PTY and real TigerVNC used the common scoped gateway plus application
probe. Terminal actual startup+operation+Surface readiness measured **207ms**;
GUI measured **977ms**. The Terminal probe returned exact actual shell output,
twice identically, before viewer input/resize/reconnect. The GUI probe returned
one measured full-frame fingerprint, identically on repeat GET, and actual
keyboard input changed pixels with local terminal echo disabled. These are
individual samples, not statistics. Neither is a Hosted staging measurement.
JSON evidence is in `evidence/native-probe-20261024/`.

After assembly relocation, the Linux app + netd all-target suite passed all
119 tests (9 application, 110 transport), with all-target locked clippy clean.
The initial remote scratch export retained three deleted files and was rejected
for duplicate binary names/unresolved old imports; removing exactly those stale
export files reproduced the committed application layout. No code workaround or
architecture-rule relaxation was needed. Remote source also includes the
subsequent Native launch-wire branch; application/transport sources are the
same as this PR. GitHub architecture CI passed on `e812a33aa`.

## Boundaries remaining

This does not install the binary in shared Runner services, advertise Native
capability, remove the runtime admission guard or issue a Hosted K receipt.
Native catalog still needs binding into the launch-spec digest; the Runner
must own producer, probe and gateway handles together and refuse publication
on workload exit/failed K/unconfirmed cleanup. A frozen result alone never
proves continuing health. API/PWA Native publication and persistence acceptance
remain separate work.

Exact-frame K is suitable only for deterministic GUI output; changing cursors
or other visual state can change its fingerprint. An application needing a
semantic operation result should expose an appropriate independently declared
Protocol/Port observation, rather than weakening K or rewriting its expected
hash from this Run. The present terminal probe supports a bounded line profile;
full control-sequence TUI probing is not claimed. Viewer transport still supports
its existing bounded input. Upload/download/clipboard remain disabled.

No production deployment, paid GPU, operator key or host desktop exposure was
performed. The first implementation's stale Unix readiness deadlock was caught
by the actual-byte test and fixed using nonblocking readiness clearing without
cancelling partial frame reads. The earlier no-planning compilation regression
was fixed in parent #1510; tests were not hidden or skipped.
