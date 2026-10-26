# Managed embedding claimed-lease CPU fixture

> Status: historical evidence / past plan. Language: en.
> Preserved content; relative navigation updated. Current [documentation map](../../../README.md).

This change adds an opt-in Linux acceptance fixture for the existing
`ConnectedWorker::execute_lease` evaluator. It does not replace an installed
Runner, alter capability advertisement, deploy a service, enable a flag, create
an owner Run or use a live API key.

## Dependencies and scope

The review base is `integration/managed-embedding-hosted-v1` at
`e42754a3a84eeb78273c540ce09205a01ff81a3c`. It combines the managed relay
(#1526), owned Native helper (#1530), and local capture policy code (#1531).
The fixture's companion Broker is the production router exercised by
`tests/managed-embedding-sdk/run.mjs` in the API repository (PR #854/#856 chain).

Real: workspace artifact materialization and digest checking, runtime binding
redemption HTTP, two pinned OCI containers, their private network and local
relay, TLS to the production Broker router with its isolated D1, lease-scoped
Surface observation gate, shared Rust HTTP Contract verification, stop report,
Run journal settlement, and labelled-resource absence after stop.

Fixture: control-plane HTTP transport, Runner credential, Broker policy/key,
model response, and the manually constructed K/D/launch spec. The specimen
uses standard-library Python; the separate #1526 fixture exercises the official
OpenAI SDK. Every outbound provider fetch in the companion is restricted to its
fixed embedding stub, so paid provider calls are zero.

This is not real staging Hosted acceptance, portable bundle validation,
D-to-launch-spec compiler validation, canonical browser lifecycle acceptance,
or proof of persisted receipt acceptance by the real control plane. K is fixed
across the positive and negative cases; D changes because the declared
`REPORTED_DIMENSIONS` environment value changes. No transport digest is
represented as a semantic workspace-tree identity.

## Boundary checks

- Only the exact lease workspace digest path and exact lease reporting paths
  are served. Both ready and stopped execution IDs must equal the digested
  launch spec. The assigned fixture Runner bearer is required.
- The backend receives only its declared runtime binding pair. The relay keeps
  the Broker capability outside container inputs; no operator key is supplied
  to either container.
- Unsigned HTTP Surface observation is refused with 403; signed logical-Port
  observations include the private backend and the public application.
- Ready evidence must name both OCI services and their observed principals.
  Receipt evidence uses the existing, smaller Rust receipt schema, which does
  not contain those principal objects.
- Broker lease authority is revoked on completion and on evaluator errors.
  Resource/journal checks run before propagating evaluator/server errors.
- Success requires all two original Contract observations. The negative case
  deliberately serves a wrong backend body while frontend HTTP remains 200.
- A test-only Linux thread-local HTTP client accepts the fixture CA, with an
  unwind-safe restoration guard. Production builds retain the ordinary TLS
  client and gain no custom trust-root option.

## CPU measurements

Host: existing `ubuntu-sugamo`, Linux amd64, existing Docker daemon. No packages,
services, live Runner credentials or shared Runner binaries were changed.

Image:
`python@sha256:c4634f578a412db396771b61b064c6e546c9d6414c7fb5b1b05d5871f1885f7b`.

| Case | Actual lease lifetime | Rust verification | Cleanup |
| --- | ---: | --- | --- |
| Both components satisfy K | 7,673 ms | 2 satisfied; fully_satisfied=true | container/network/journal empty |
| Wrong backend body; frontend 200 | 7,837 ms | backend failed; frontend satisfied; fully_satisfied=false | container/network/journal empty |

Both receipts retain ContractRef
`sha256:52fa018c3a2939d2bfb73df54919d868660cb3b441dc40e7661ff6f739d9a81f`.
These are single local CPU-fixture measurements, not staging latency claims.
Python does not handle termination gracefully in this specimen: both containers
were force-stopped with exit 137 and machine-confirmed removed.

The build source manifest verified 1,418 retained files, SHA-256
`91fe2e4dba3f3c90f6ab165171b3cb856d8bb8c8a18f024b116d1558382094e6`,
followed by test-file overlay SHA-256
`05b8d8e24be11cd08b94eeaed65b4d8d52b9770e1dad9bd114d67bfd03721add`. Documentation, images and
large non-build artifacts were omitted from the transfer. All Rust/Cargo
inputs had fresh mtimes before the shared target was used; Cargo ran offline,
locked, with two jobs and incremental/debug information disabled.

## Regression checks

- Linux worker library: 162 passed, 0 failed, 16 ignored by default; the new
  ignored CPU fixture was explicitly executed twice as reported above.
- Linux worker all-target Clippy with `-D warnings`: passed.
- macOS worker library: 161 passed, 0 failed, 14 ignored (Linux fixture excluded).
- macOS worker all-target Clippy with `-D warnings`: passed.
- Format and diff whitespace checks: passed.

The initial Linux regression invocation hit the process-local 1,024 descriptor
limit. The next invocation exposed three state-session tests using a library
test executable as their sandbox shim. The existing documented harness requires
`ulimit -n 8192` and `ATO_TEST_WORKER_BIN` pointing at a same-source built Worker;
with those conditions all 162 passed in 18.03s. No skip or code workaround was
introduced, and neither setting changed an installed service or host-wide limit.

## Reproduction

Start a fresh instance of the API companion with `ATO_SDK_FIXTURE_SERVE=1`.
Copy only its mode-0600 `fixture.json` and public `ca.pem` to an owned Linux
test directory. Keep the private CA key local. An SSH loopback-only reverse
forward may connect the fixture's localhost HTTPS origin. Use a fresh fixture
and a fresh test environment root for each case; completion revokes its lease.

Set `ATO_MANAGED_SDK_FIXTURE_DIR`, `ATO_MANAGED_HOSTED_TEST_ROOT`, and
`ATO_MANAGED_HOSTED_TEST_IMAGE` (a pullable digest-pinned Python image), then run:

```sh
cargo test --offline --locked -p ato-connected-realization-worker --lib \
  actual_managed_embedding_lease_materializes_redeems_verifies_and_stops \
  -- --ignored --nocapture
```

For the negative case set `ATO_MANAGED_HOSTED_BAD_COMPONENT=1`. Keep ATO_HOME,
TMPDIR and ATO_EXPLORATION_TEST_ROOT in a fresh owned test root. Stop the own
companion/SSH forward and remove the short-lived capability copy afterwards.
No missing credentials, paid provider availability, or manual browser login
should be bypassed to reinterpret this fixture as staging acceptance.

## Remaining acceptance

Startup failure/Broker rejection/ready-ACK failure need separate expected-error
cases. Ordinary unit tests cover other scoped-assertion failures; this combined
fixture currently checks unsigned denial and the authorized private path.
Actual owner-created staging source intake, installed Worker claim, real
Hosted receipt persistence, and manual staging browser lifecycle remain separate.
