# 6b-A qualification: version-pinned package-manager-aware workspace build

Baseline #1436 merged at `d63345cc236f6eb036b25fdf56383e71c680cdc2`.
The immutable 20-app cohort and all eight affected archive hashes are preserved.
This is **source qualification only**, before implementation/installation/remeasurement.
Typed-K success remains **1**, functional application success established **0**.

| App | Exact manager evidence | Root build | Qualification/output |
|---|---|---|---|
| Excalidraw | yarn 1.22.22, packageManager | yarn --cwd ./excalidraw-app build | Frontend candidate, workspace output excalidraw-app/build |
| SearXNG | none; no root lock | none | Python service, theme assets searx/static/themes/simple |
| Etherpad | pnpm 12.4.2, packageManager | none | Node service; admin and OIDC assets, not standalone static |
| Gitea | pnpm 12.6.0, packageManager | none (Makefile) | Go backend plus public/assets |
| HedgeDoc | pnpm 11.24.0, **devEngines.packageManager** | dotenv / turbo run build | Frontend server + backend, not standalone static |
| Actual Budget | yarn 4.17.1, packageManager and yarnPath | lage build | Browser candidate in packages/desktop-client/build; multi-product workspace |
| JupyterLab | yarn 3.5.0, packageManager | npm run build:dev | dev_mode/static assets need Python server/kernel |
| Grist core | yarn 1.22.22 + declared sha512 integrity | buildtools/build.sh | Node/Python server; static assets and _build |

The machine-readable companion records exact declaration bytes, workspaces,
lock kinds, scripts, output evidence paths and hashes. Seven have declared exact
manager versions; that does **not** make seven valid static applications.
SearXNG's root refusal is not evidence of an unrecognized Yarn/pnpm lock.
All require external dependency resolution on an empty cache; this is source
inspection, **not** a measured install result. No network permission is added.

## Reuse before adding a primitive

NodeEvidence already detects all relevant lock kinds. Formation also already has
`StaticBuildProfileV1`, `ResolvedPackageManager`, `resolve_toolchains`, and
`execution::InputFacts`. The next implementation must connect/reuse these, rather
than adding a second lock detector or parallel package-manager resolver.

Preserve npm and the versioned node-static/v1 contract. New support must carry an
exact manager version/generation through authoring and execution, refuse
ambiguous declarations, retain immutable install modes, and explicitly qualify
workspace/script/output rather than guessing `dist/` or treating backend assets
as a complete app. Bun remains deferred. No repo-name branches.

After implementation, rerun the same eight pinned archives. Report separately:
1. authoring reach (including transition to network_denied),
2. actual same-K verified coverage,
3. functional product capability (not inferred from GET / 200).

Implementation and remeasurement are **pending**. Model calls 0, no deploy or
remote migration. No 50/100-app completion claim.

## Baseline CI review

#1436 exact head df033dd9a4307d6d38f636d877b817e5d75e7217 was reviewed before
merge. Rust CI 36513258242 remains red: Windows Unix-only browser_sandbox APIs,
Ubuntu hosted Python/Node, macOS portable ownership/hosted validator failures;
these match the previously recorded same-base failures. macOS Browser E2E also
failed: recorded as separate timing instability, **not** base-reproduced. No
Browser/connected-realization-worker source changed in #1436. CodeQL passed;
actual 20-app evidence and local helper checks are separate from CI status.
