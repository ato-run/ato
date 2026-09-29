# Formation 6b-D0 — service/process source qualification (41 apps)

Status: **qualification only**. No implementation, execution, install or
model call; no deploy or remote migration. Base `461fa78c`.

Input: the 41 non-success sources of the [50-app wave](formation-coverage-50.md)
whose preregistered shape is a service/process (6 static frontends excluded).
Same pinned archives, nothing re-fetched. Facts come from a static read of root
manifests, lockfiles, version files, Dockerfiles and compose files
(`scripts/acceptance/coverage/service-qualification-facts.py`) plus targeted
file reads. Judgments are recorded per field in [JSON](formation-service-qualification-d0.json).

Runtime catalog today: Node 20.20.2 / 22.14.0, Python 3.11.11 / 3.12.7 /
3.13.1. Nothing else.

## Families

Python service 11, multi-service 11, Go 9, Node service 4, PHP 3, Rust 1, JVM 1, .NET 1.
By primary language the catalog gap is Go 9, PHP 3, Rust 2, Ruby 1, JVM 1,
.NET 1 (Lemmy and Mastodon are counted under multi-service above).

| App | Family | Typed native launch | Runtime in catalog | Deps locked | External service | 50-wave terminal |
|---|---|---|---|---|---|---|
| WBO | Node service | ✓ | ✓ | ✓ | – | `preset_node_static_needs_build_script` |
| FreshRSS | PHP | – | ✓ | ✓ | – | `preset_node_static_needs_build_script` |
| linkding | Python service | – | ✓ | ✓ | – | `network_denied` |
| SearXNG | Python service | – | – | – | – | `preset_node_static_needs_lockfile` |
| changedetection.io | Python service | – | – | – | – | `preset_no_match` |
| Etherpad | Node service | – | – | ✓ | – | `preset_node_static_v2_workspace` |
| Miniflux | Go | ✓ | ✓ | ✓ | required | `preset_no_match` |
| Gitea | Go | – | ✓ | ✓ | – | `preset_node_static_v2_workspace` |
| Vikunja | Go | – | ✓ | ✓ | – | `preset_no_match` |
| Mealie | Python service | ✓ | – | ✓ | – | `preset_no_match` |
| HedgeDoc | multi-service | ✓ | – | ✓ | required | `preset_node_static_v2_workspace` |
| JupyterLab | Python service | ✓ | ✓ | – | – | `preset_node_static_v2_workspace` |
| LibreChat | multi-service | – | – | ✓ | required | `preset_node_static_v2_bun_deferred` |
| ComfyUI | Python service | – | ✓ | – | – | `preset_no_match` |
| PdfDing | Python service | – | – | ✓ | – | `network_denied` |
| Grist core | multi-service | ✓ | – | ✓ | – | `preset_node_static_v2_manager_config` |
| File Browser | Go | – | ✓ | ✓ | – | `preset_no_match` |
| Wiki.js | Node service | ✓ | – | ✓ | – | `preset_node_static_v2_manager_config` |
| Kutt | Node service | ✓ | ✓ | ✓ | – | `preset_node_static_needs_build_script` |
| Umami | multi-service | ✓ | – | ✓ | required | `preset_node_static_v2_workspace` |
| Ghost | multi-service | – | – | ✓ | required | `preset_node_static_v2_workspace` |
| Radicale | Python service | ✓ | ✓ | – | – | `preset_no_match` |
| Calibre-Web | Python service | ✓ | ✓ | – | – | `preset_no_match` |
| Shynet | Python service | – | – | ✓ | required | `preset_node_static_needs_build_script` |
| text-generation-webui | Python service | – | – | – | – | `preset_no_match` |
| PrivateGPT | multi-service | ✓ | ✓ | ✓ | – | `preset_no_match` |
| Gotify | Go | – | ✓ | ✓ | – | `preset_no_match` |
| Navidrome | Go | – | ✓ | ✓ | – | `preset_no_match` |
| PocketBase | Go | – | ✓ | ✓ | – | `preset_no_match` |
| Glance | Go | – | ✓ | ✓ | – | `preset_no_match` |
| Shiori | Go | ✓ | ✓ | ✓ | – | `preset_node_static_v2_bun_deferred` |
| Vaultwarden | Rust | – | ✓ | ✓ | – | `preset_no_match` |
| Lemmy | multi-service | – | ✓ | ✓ | required | `preset_no_match` |
| Paperless-ngx | multi-service | – | ✓ | ✓ | required | `preset_no_match` |
| Mastodon | multi-service | ✓ | ✓ | ✓ | required | `preset_node_static_v2_workspace` |
| Linkwarden | multi-service | – | – | ✓ | required | `preset_node_static_v2_workspace` |
| NetBox | multi-service | ✓ | ✓ | – | required | `preset_no_match` |
| BookStack | PHP | – | ✓ | ✓ | required | `network_denied` |
| Firefly III | PHP | – | ✓ | ✓ | required | `preset_node_static_needs_build_script` |
| OpenRefine | JVM | – | ✓ | – | – | `preset_no_match` |
| Jellyfin | .NET | – | ✓ | – | – | `preset_no_match` |

## Aggregate

- Typed native launch declaration (package `start` script, pyproject console
  script or Procfile): **15/41**. The rest declare launch
  only as a Dockerfile CMD/entrypoint (container metadata for an OCI Adapter,
  not a native launch) or not at all.
- Runtime resolvable into the current catalog: **27/41**.
  Several Node apps pin an exact Node the catalog lacks (22.12.0, 22.23.3,
  24.x); Mealie needs Python 3.14.
- Dependency lock present: **31/41**. Every family needs
  dependency resolution to install; no offline cache is preregistered.
- Mandatory external service: **13/41**
  (Postgres/MySQL/Redis/Mongo/RabbitMQ). Many others default to SQLite or a
  local directory, i.e. persistent state, not a second service.
- **Port provable from a native declaration: 0/41.** Ports appear only as
  Dockerfile `EXPOSE`, an env var default (`PORT`) or a Procfile `$PORT`.

## First implementation candidates (Node/Python, not selected)

Filter: Node or Python family, typed native launch, runtime in the catalog, no
mandatory external service.

- **WBO** (Node service): package script start: node ./server/server.mjs (plain argv). Remaining: port only via PORT env default (no declared port contract); npm install needs dependency resolution (network); persistent board data dir.
- **JupyterLab** (Python service): pyproject console script (jupyter-lab). Remaining: source install requires a Yarn 3 workspace frontend build (manager config gap); Python deps unlocked; kernel processes.
- **Kutt** (Node service): package script start: node server/server.js --production (plain argv); separate migrate script. Remaining: two-step typed launch (migrate then start); better-sqlite3 native module needs a compiler in the build sandbox (unverified); npm install needs dependency resolution; PORT via env.
- **Radicale** (Python service): pyproject console script: radicale. Remaining: no dependency lock (unpinned pyproject deps); pip install needs dependency resolution; port only as container EXPOSE / CLI flag.
- **Calibre-Web** (Python service): pyproject console script: cps. Remaining: requirements are ranges, not a lock; requires a Calibre library state; port not declared.

None is free of the dependency-network gate, and none has a native port
contract. So the measured order of blockers for a first service route is:
(1) a typed launch + port binding that is declared, not inferred;
(2) dependency acquisition policy, without which every candidate stops at
`network_denied`; (3) then execution-capability details (native modules,
two-step launch, state). Go/PHP/Rust/Ruby/JVM/.NET remain Runtime catalog gaps:
authoring for them would not raise verified coverage until a runtime exists.

## Never guessed

No port from a framework, start script or README; no argv from `main.py`,
`cmd/`, a framework name or README shell; Dockerfile CMD/EXPOSE is not treated
as a native launch/port declaration. Absent a typed declaration the correct
result remains an authoring refusal.
