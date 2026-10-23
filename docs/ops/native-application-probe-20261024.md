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

## Verification

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
