# Formation current support — 2026-09-30

Current code: ato main `b43eaa0c55be0229d55b8d82054d355b616d04b1`,
ato-api main `dcc3049a69051ed6d1c7819580ed1e171815c34c`, checked against live
GitHub at the start. #1448 is merged; #1449 started open at
`b192fe704f92650d480f4345aeea3e91efae673e` and remains a docs/evidence PR.
These current code pins do **not** replace historical measurement pins.

| Source shape | Formation entry | Required runtime / policy | Fixture verification | Pinned upstream verification | Functional / persistence | Merge / deployment |
|---|---|---|---|---|---|---|
| Single HTML / completed root-index site | Automatic known-D: single-html/v1 or static-files/v1 | Static materializer/server; detector eligibility still applies | Actual static substrate / receipt acceptance recorded | 2048, reveal.js fresh static receipts at original binary pin | Fixed game keys / slide next-and-back PASS in new append; Ato state unmeasured | Implemented/merged; new evidence unmerged; no track rollout |
| One React `.jsx` file | Automatic known-D: single-jsx/v1 | Provisioned JSX compiler | Narrow compiler path; not rerun here | No new upstream repository qualification here | General React repository/TSX coverage not established | Implemented/merged; deployment not reverified |
| Node-built static Web | Automatic npm v1 or bounded pnpm/Yarn v2 | Compatible Node/manager; explicit dependency network permission; lock/build/config checks | v1/v2 acceptance recorded | Eight-app v2 survey: all authoring refusals; no added verified/functional app | No functional increment from 6b-A | #1440 merged; no track rollout |
| Static frontend inside monorepo | Explicit owner-authorized workspace operation / typed proposal, not changed automatic selection | `node_static_workspace@1`; eligible opaque workspace ID, root install and workspace build | Actual bounded workspace fixture acceptance | Six upstream workspaces: zero published operations | No upstream functional/persistence increment | #1441 merged; no track rollout |
| Small Node/Python HTTP service | Existing authored D / explicit authoring; bounded proposal vocabulary (e.g. Python HTTP entrypoint) | Declared launch/port, pinned runtime/dependencies, explicit effects and network | Actual process Runtime/Verifier; 5b zero-known-D and 5c one-round integration | General OSS service coverage is not established by fixture success | Same-K fixture PASS is not broad functional/persistence coverage | 5a/5b/5c implemented, verified, merged; no track rollout |
| Verified OCI image | Explicit image/port/resource/state authoring | Managed OCI Adapter, digest/platform, readonly root, capabilities denied, explicit state | Existing OCI execution/state integration | Separate portable-application evidence exists; it is not automatic source Formation coverage | No new service functional acceptance here | Implemented/merged; no track rollout |
| Frozen source + root Dockerfile | Explicit source-to-OCI request; default target, frozen external image graphs; not a universal known-D preset | Isolated bounded builder; separate acquisition/build allowlists; current Cmd/state constraints; verified artifact then existing OCI run | Offline isolation/hardening and bounded online checks recorded | WBO: image/verified artifact PASS, start refused; Vikunja unexecuted; three new candidates unexecuted | WBO C/D/E unreached; no service functional/persistence increment | #1446/#1447/#1448 and API #710 merged; no track rollout |

“No track rollout” describes this Formation track's authorized work. It does
not claim that previously shipped HTML/JSX delivery is absent from the product.
API/executor existence alone does not establish automatic Formation support.
The table is based on code and linked prior acceptance, not a new full
qualification of every entry.

`lib/formation/src/preset.rs` checks Node build metadata before its ordinary
root-index fallback. A `package.json` with absent lock/build metadata may be
refused even if `index.html` exists. pnpm/Yarn recognition is intentionally
narrow; it does not certify arbitrary scripts as static. Workspace operations
must already be qualified/authorized. CandidateProducer cannot create missing
runtime, network permission or external services.

The final 50-app baseline remains **typed-K 3, functional established 0** at
`c4c285f3`, producer/provider OFF and network denied, Linux/aarch64. Uptime
Kuma's static fallback is not the monitoring application. LibreChat's routing
regression was fixed in #1443, merge `233bda52e7e1f3c9acd4a67d41974f2152f87f9d`;
the separate rerun restored K/D then `network_denied`, without a new receipt.

The [new static append](formation-static-functional-append-2026-09-30.md)
establishes the fixed UI operations for **two unique apps**, with fresh same-K
receipts. This does not turn the old wave into a new “2/50” functional rate.
No Ato persistence acceptance was executed.

The [three-candidate survey](formation-oci-candidates-2026-09-30.md) selects
none. ENTRYPOINT-only and explicit state outside VOLUME remain possible small
profile changes, but neither establishes a complete viable candidate here.
No profile extension, source rewrite or online build was performed.

## Open docs PR review and CI boundaries

Review of #1449's initial exact head found a numerical overclaim: xgo's
4,161,691,592 **compressed** bytes exceed the 200 MiB acquisition and 500 MiB
transfer bounds, but do not by themselves prove exceeding the 5 GiB job disk.
The preregistration now distinguishes unmeasured expanded disk usage. Vikunja
remains **unexecuted**, rather than a build failure. Its old truncated platform
manifest ID is retained as a provenance limitation; no new GHCR access was made.
WBO's two online builds and missing run-2 transfer total remain explicit in the
append. Historical baseline/plan/results are unchanged.

#1449 initial Rust CI run [36653416584](https://github.com/ato-run/ato/actions/runs/36653416584)
actually executed and failed; this is not a billing failure. Compare its exact
parent `64d029cb` run [36605388146](https://github.com/ato-run/ato/actions/runs/36605388146):
Windows Unix API compilation, macOS process ownership / five hosted-validator
tests, and Ubuntu hosted Node/Python failure recur on the parent. Two additional
head-run failures (volume-backed session and reserved-path verifier) are **not
reproduced in that paired parent log**, so are not asserted to be base-reproduced.
The initial PR changed only two JSON docs; Rust/lock/workflow trees are identical
to the parent. Current main run [36654141449](https://github.com/ato-run/ato/actions/runs/36654141449)
also fails with additional runtime/port-registry tests. None is reported green;
branch protection settings remain unchanged.

The [reviewed-head CI recheck](formation-ci-static-recheck-2026-09-30.md)
records Rust CI run `36658258823` at `06330897500092555c64cf143b241fbf93770a8d`.
Its additional Ubuntu static-state-token failure is **base-reproduced**:
exact base `b43eaa0c` CI has the same source location and ConnectionReset, and
an independently compiled isolated base run also fails there. The three
isolated head runs were FAIL / PASS / PASS; the base runs were cached PASS,
then independently compiled FAIL. This is intermittent observation, not an
established timing cause or a test fix. The user authorized merging this
lineage after the recheck, including admin merge after an exact-head check.

No deploy, remote migration, extra model call or credential reread; #1421
untouched. The 4,513,388 USD-micros future reservation is unchanged and is not
new call authorization. sugamo at start: approximately 30 GB free,
`~/ato-d2` approximately 11 GB. Existing artifacts/state/evidence were preserved;
no host-wide Docker prune.
