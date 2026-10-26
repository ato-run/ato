# Formation 6a — 20-app known-D baseline results

20/20 pinned upstream archives traversed the existing local Formation path on isolated Linux/aarch64. CandidateProducer and DecisionProvider OFF; model calls **0**. Preregistration was committed before measurement. No per-app authoring overrides.

## Result scope

- **1 typed-K success**, with an actual Runtime/Verifier receipt: Uptime Kuma via the existing static-files fallback. **This is not proof the monitoring app works.** It verifies only GET / 200 and frozen source identity. Functional application successes established: **0**. Browser/backend/persistence remain unverified.
- **16 known-D/authoring** terminal refusals.
- **3 effect/policy** (`network_denied`) terminal refusals. Workload network permission was not expanded.
- Source acquisition was authorized separately; denied workload networking is not a claim dependency provisioning is universally unsupported.

| App | Primary | Observed result |
|---|---|---|
| Excalidraw | known-D/authoring | preset_node_static_needs_lockfile |
| WBO | known-D/authoring | preset_node_static_needs_build_script |
| Uptime Kuma | success | static typed-K receipt; app functionality unverified |
| FreshRSS | known-D/authoring | preset_node_static_needs_build_script |
| linkding | effect/policy | network_denied |
| SearXNG | known-D/authoring | preset_node_static_needs_lockfile |
| changedetection.io | known-D/authoring | preset_no_match |
| Etherpad | known-D/authoring | preset_node_static_needs_lockfile |
| Miniflux | known-D/authoring | preset_no_match |
| Gitea | known-D/authoring | preset_node_static_needs_lockfile |
| Vikunja | known-D/authoring | preset_no_match |
| Mealie | known-D/authoring | preset_no_match |
| HedgeDoc | known-D/authoring | preset_node_static_needs_lockfile |
| Actual Budget | known-D/authoring | preset_node_static_needs_lockfile |
| JupyterLab | known-D/authoring | preset_node_static_needs_lockfile |
| LibreChat | effect/policy | network_denied |
| ComfyUI | known-D/authoring | preset_no_match |
| PdfDing | effect/policy | network_denied |
| Grist core | known-D/authoring | preset_node_static_needs_lockfile |
| File Browser | known-D/authoring | preset_no_match |

## Next 6b generic fix

Largest concrete refusal: `preset_node_static_needs_lockfile` (8). Select version-pinned Yarn/pnpm-aware build/authoring support, not repo-name branches. Qualify real static output vs service/monorepo before admission. Keep denied-network semantics: eliminating the lockfile refusal may expose a later policy failure rather than improve verified coverage. Remeasure affected old apps and report zero gain honestly before expanding the cohort. Not implemented yet.

Exact pins, archive/license hashes, original and replacement candidates, Runtime profile, attempts, D/K refs, receipt and retained ref are in JSON. Pre-K failures carry null K/D rather than fabricated identities. Measurement covers one actual local Runtime profile; it is not broad functional/production coverage. No deploy/remote migration/#1421 change.
