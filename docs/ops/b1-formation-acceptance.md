# B1 Formation Service v1 — acceptance closeout

**Status: B1 implementation and acceptance are COMPLETE. B1 rollout is NOT.**

These are different claims and this document keeps them apart:

    B1 implementation / acceptance    COMPLETE
    B1 rollout                        shadow — Formation is not primary anywhere

The final staging rollout policy is `static: shadow`, `python: shadow`. Every
Formation result on staging is compared and discarded; the existing paths still
form everything that ships. "The Formation architecture is finished" and
"Formation was cut over" are separate sentences, and only the first is true.

After this record, B1 is closed. Further work on Formation is a new,
explicitly-scoped change — not "adding to B1".

The detailed evidence lives in
[`b1-formation-staging-acceptance.md`](./b1-formation-staging-acceptance.md);
this is the record you read first.

---

## What was accepted

    GitHub source
      → pinned source closure
      → detector evidence
      → Program Intent
      → Effective Build Plan
      → isolated build
      → immutable materialization
      → FormationResult
      → ComputeSchema registration
      → P3 RuntimeLaunchSpec
      → Run

## Accepted revisions

| | |
|---|---|
| `ato` nightly baseline | `f637974e6c061227adcbbf6a5a53b4b87a59ea1a` |
| `ato` accepted head | `7e52440b` on `feat/formation-source-closure` |
| `ato-api` nightly baseline | `c4d30f942b0781719f693ce9b8fa6290f6360904` |
| `ato-api` accepted head | `72a2049` on `feat/formation-registry` |
| Migrations | `0195`–`0201`, applied to staging and name-matched |

## Accepted staging deployments

| deployment | version id | what it carried |
|---|---|---|
| `ato-store-staging` | `da29f393-aa56-489b-9c3b-4ceae55d8a2f` | Scenarios A–L |
| `ato-store-staging` | `89715814-c492-4593-b5b1-53f07ea6d0fb` | shadow mode + the classified comparator (B1-S §1–§8) |
| `ato-store-staging` | `4e6859fd-1563-4439-bfae-be741718d297` | comparator determinism fix (B1-S §9) |

The Formation worker ran from `ubuntu-sugamo`, `/home/ekohsuke/src/ato-p37`,
built from the accepted `ato` head.

## Scenarios A–L — all PASS

| | Scenario | Result |
|---|---|---|
| A | contract | forbidden fields (incl. a nested `secret_value`), mutable ref, escaping subdirectory, `publish_enabled`+network, `oci_image`, unknown protocol — all typed refusals against the real API |
| B | source pinning | a full commit is required; `main` and a short SHA are `ATO_ERR_FORMATION_SOURCE_NOT_PINNED` |
| C | closure identity | the same tree from two different archives yields one closure; the closure is neither the archive digest nor the tree digest |
| D | idempotency | a resubmitted key returns the same job |
| E | stale attempt | `409 formation_attempt_superseded`; a duplicate completion is `409 formation_result_already_accepted` |
| F | build sandbox | source read-only, output writable, host sentinel invisible, env not inherited, denied network genuinely fails, allowed network reaches the index |
| G | Static lane | a pinned source formed into a manifest/receipt/blobs bundle by the canonical materializer; schema registered as `static_web`, no process realization, **0 Runner runs** |
| H | Python lane → Run | unattended worker: claim → pinned fetch → build → publish → schema; then `{}` in, `{"status":"ok","db_path":"/data/app.sqlite"}` out |
| I | state continuation | three Runs of one instance, fences 1→2→3, revisions chained, `note-A` survives to the third |
| J | tenant isolation | B and C start empty on the same schema; B never sees A |
| K | existing artifact compatibility | both Static instances open unmodified, P0 bridge live, the untouched one has **0 runs** |
| L | rollback | off → shadow → primary-unallowlisted (degrades to shadow) → primary-allowlisted → off, then an existing instance still runs with its state intact |

## B1-S shadow comparison — all PASS

The cutover gate: does switching to Formation change what the software MEANS?
Not "does the digest match" — a digest comparison would have passed three of
the four defects below while the artifact silently changed.

| gate | verdict |
|---|---|
| §1 Static shadow — Fixture C, `gabrielecirulli/2048@478b6ec` | PASS |
| §7 Static shadow — Fixture A, `thedoggybrad/2048ontheweb@c71efbb` (plain HTML) | PASS |
| §7 Static shadow — Fixture B, `ato-run/ato-e2e-static-spa@1e1be10` (Vite built) | PASS |
| §14 Fixture B browser smoke | PASS |
| §8 Python semantic compare — `ato-run/ato-e2e-compute-server@4f442f1` | PASS |
| §3/§6 Shadow side-effect zero | PASS — 0 across 7 attempts |
| §4 Shadow failure isolation | PASS |
| §9 Comparator determinism | PASS |
| §10 Rollback drill | PASS |

### UNEXPECTED SEMANTIC DRIFT = 0

Every remaining difference is classified and recorded: bridge assets (the
legacy single `replay-bridge-v0.js` versus the canonical P0 pair), the
`index.html` blob that follows from them, Fixture A's `LICENSE.txt` (the
existing path began admitting `text/plain` between 2026-08-11 and 2026-09-02 —
proven by two live artifacts of the SAME pinned source differing by exactly
that file), and the Python lane's now-explicit runtime.

## Four silent regressions this gate found, and fixed

None would have failed a digest comparison. Three would have shipped a working
page that was quietly the wrong software.

**1. The canonical materializer could not type `ico` / `woff` / `eot`.**
Staging is serving an artifact whose manifest names all three. Re-forming that
same source through the canonical materializer dropped a favicon and six font
files and reported success — a degraded site, not a dead one. Not an I0
transplant regression: `origin/main`'s table was identical. The wider table
belonged to the legacy Builder, so **canonical code was not the same thing as
the historically accepted artifact contract.**

**2. The Formation Static lane reached the producer without extracting.**
Extraction is where the browser-runner and instance-state bridges are injected,
so its bundles carried neither. The site would have looked correct and silently
lost its Browser Instance State. `produce_static_web_bundle` now takes
`&ExtractedStaticWebOutput`, so the bypass no longer compiles.

**3. Static had no build lane.** `plan_build` dispatched
`(Lane::StaticWeb, _) => {}`, so a generated site published whatever stale
output happened to be committed. Fixture B publishes `STATIC_FIXTURE_V2` from a
checked-in `dist/` where the existing path published a real Vite build of the
source, `STATIC_FIXTURE_V1` — a different application, reported as success.
Closed by Static Build Profile v1, which reproduces the existing Builder's own
gate and never treats "a `dist/` exists" as evidence of anything.

**4. The Python lane refused a stdlib program for want of a DEPENDENCY
marker.** Not for want of authoring, which was present. Requiring the second is
a rule that says every Python program must have dependencies.

## Support matrix

    Static, no dependency resolution        public untrusted allowed
    Static, dependency-resolving build      trusted / allowlist only
    Python, ANY (dependencies or not)       trusted / allowlist only

### Why Python is ANY

A real constraint, not a rounding. The Python lane always provisions its own
interpreter — the host's `python3` is never the answer, because which
interpreter built an artifact must be a property of the source rather than of
whichever machine claimed the job. Provisioning needs the network, and the
policy enum has no class between `denied` and `dependency_resolution`. So a
stdlib-only program with no dependency graph to resolve must still request
`dependency_resolution`, and lands in the trusted row.

ADR-018 is not bent for this: `publish_enabled` together with
`dependency_resolution` is still refused, so untrusted public sources cannot
take either dependency-resolving path.

### Future work — a third network class

Lifting the Python row means a `toolchain_provisioning` network class: a fixed,
pinned URL set rather than arbitrary egress. That would let dependency-free
Python join Static in the public-untrusted row. It is a policy-model change,
deliberately outside B1, and B1 ships with the limitation stated rather than
papered over.

## Not claimed

- Nothing is deployed to production. `ato.run` is untouched.
- Formation is primary for nothing. Final policy: `static: shadow`,
  `python: shadow`.
- The live artifact `swm_iuxlwt2dqf3km…` still carries manifest digest
  `sha256:fd471ae7…` — byte-for-byte what it was before B1-S began.

## The rule this gate established

    Do not treat code as correct because it is canonical.
    Compare live artifact + intended semantics + current implementation.

Reading only the code would have shipped defect 1. Treating a single live
artifact as the reference would have called Fixture A's `LICENSE.txt` drift.
Only the three-way comparison gets both right.
