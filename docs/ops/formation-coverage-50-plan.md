# Formation 6b-C — 20 → 50 coverage preregistration

Status: **measurement designed, preregistered**. The 30 new apps were chosen,
pinned and license-reviewed before any Formation result for them was seen.
Not yet measured at this commit.

- Measurement code: merged main `c4c285f3` (#1441). Only harness/docs differ;
  the harness refuses to run if any Rust source differs from that pin.
- Original 20: [6a plan](formation-coverage-20-plan.json) unchanged (same
  archives, hashes re-checked). The 6a ledger is not overwritten.
- Policy: existing `coverage_baseline` → `local::run`, Contract inferred from
  source (authored `capsule.toml` or existing presets), CandidateProducer OFF,
  DecisionProvider OFF, model calls 0, **network denied**, max 4 attempts,
  default source/build limits, 900 s hard process timeout (recorded, never a
  success). No per-app capsule.toml, D, workspace selection, build command,
  output override, network grant or Binding.
- Success = actual `fully_satisfied` same-K receipt. The current K is GET / =
  200 + source identity; it never establishes functional application success.

## Replacements before execution (license review only)

- TandoorRecipes/recipes → NetBox: AGPL-3.0 with a Commons Clause condition.
- plausible/analytics → Mastodon: `extra/` subtrees grant no rights.
- `oobabooga/text-generation-webui` is now canonically `oobabooga/textgen`.

## New 30 (indices 21–50)

| # | App | Ref | Archive SHA-256 | License | Language | Group | Expected shape |
|---|---|---|---|---|---|---|---|
| 21 | [2048](https://github.com/gabrielecirulli/2048) | `478b6ec346e3` | `4f3e35b3b9124c5a…` | MIT | JavaScript | static | static site, no build |
| 22 | [reveal.js](https://github.com/hakimel/reveal.js) | `f8c9ec3bb3b2` | `b695207d618d628b…` | MIT | JavaScript | static | static library + demo page; build optional |
| 23 | [CyberChef](https://github.com/gchq/CyberChef) | `d0267c3cf769` | `801900966adb4f72…` | Apache-2.0 | JavaScript | frontend | static SPA produced by a non-Vite build |
| 24 | [IT-Tools](https://github.com/CorentinTh/it-tools) | `d505845f918e` | `02fc371df3d79b18…` | GPL-3.0 | Vue/TypeScript | frontend | static Vite SPA |
| 25 | [Squoosh](https://github.com/GoogleChromeLabs/squoosh) | `e8d35e0fb66e` | `8a4caf8744066d08…` | Apache-2.0 | TypeScript | frontend | static SPA with custom build |
| 26 | [Hoppscotch](https://github.com/hoppscotch/hoppscotch) | `a9ffa29ce503` | `c1315ac8a04c64a2…` | MIT | TypeScript | workspace-frontend | workspace frontend + backend services |
| 27 | [Wiki.js](https://github.com/requarks/wiki) | `712a3a5bb23b` | `06e8b84154b9c4a4…` | AGPL-3.0 | JavaScript/Vue | node-service | single Node service with DB |
| 28 | [Kutt](https://github.com/thedevs-network/kutt) | `279b491b53bb` | `6fd86408c5ec9175…` | MIT | JavaScript | node-service | single Node service |
| 29 | [Umami](https://github.com/umami-software/umami) | `ec0ff50388c2` | `dfdc154f23fe52ff…` | MIT | TypeScript | node-service | server + frontend (Next.js) with DB |
| 30 | [Ghost](https://github.com/TryGhost/Ghost) | `edea774531f4` | `c85e45d82c424c4f…` | MIT | TypeScript/JavaScript | node-monorepo | Node monorepo service |
| 31 | [Radicale](https://github.com/Kozea/Radicale) | `1913df87c1d5` | `8a2438e0c5ffac4e…` | GPL-3.0 | Python | python-simple | simple Python HTTP service |
| 32 | [Calibre-Web](https://github.com/janeczku/calibre-web) | `6e840acbda8d` | `bad145af40b5fbcf…` | GPL-3.0 | Python | python-web | Python web app |
| 33 | [Shynet](https://github.com/milesmcc/shynet) | `ca35caba3af2` | `80c79ee8c1759109…` | Apache-2.0 | Python | python-web | Django web app |
| 34 | [text-generation-webui](https://github.com/oobabooga/textgen) | `c93f88712395` | `424ff940d49160b9…` | AGPL-3.0 | Python | python-ai | Python AI web UI (large deps, GPU optional) |
| 35 | [PrivateGPT](https://github.com/zylon-ai/private-gpt) | `01ac43d72bc4` | `c2f13bff0c9cfb47…` | Apache-2.0 | Python | python-ai | Python AI service |
| 36 | [Gotify](https://github.com/gotify/server) | `d02796bd084a` | `3199fdca520f9501…` | MIT | Go | go-service | compiled Go server with embedded UI |
| 37 | [Navidrome](https://github.com/navidrome/navidrome) | `3a31f702b529` | `522f487abd5146cf…` | GPL-3.0 | Go | go-service | compiled Go server + frontend build |
| 38 | [PocketBase](https://github.com/pocketbase/pocketbase) | `5cec579da984` | `efce3da1124ee8ad…` | MIT | Go | go-service | compiled Go server |
| 39 | [Glance](https://github.com/glanceapp/glance) | `372466c6d753` | `c77ca7bade309109…` | AGPL-3.0 | Go | go-service | compiled Go server |
| 40 | [Shiori](https://github.com/go-shiori/shiori) | `9a9a426acaca` | `ded75f4adf732070…` | MIT | Go | go-service | compiled Go server |
| 41 | [Vaultwarden](https://github.com/dani-garcia/vaultwarden) | `061694d0cb3b` | `ef8e81a80286282d…` | AGPL-3.0 | Rust | rust-service | compiled Rust server |
| 42 | [Lemmy](https://github.com/LemmyNet/lemmy) | `f1476db87856` | `cedd12baa456b0ad…` | AGPL-3.0 | Rust | rust-multiservice | Rust backend requiring Postgres |
| 43 | [Paperless-ngx](https://github.com/paperless-ngx/paperless-ngx) | `61f36eb253bd` | `d2d560f5f0c35e2c…` | GPL-3.0 | Python | multi-service | multi-service (web, worker, broker, DB) |
| 44 | [Mastodon](https://github.com/mastodon/mastodon) | `d95390addf6d` | `e4a7a3a320de5670…` | AGPL-3.0 | Ruby | multi-service | multi-service (web, worker, streaming, DB, cache) |
| 45 | [Linkwarden](https://github.com/linkwarden/linkwarden) | `952ac4540657` | `fb1ce9889cc1b77e…` | AGPL-3.0 | TypeScript | multi-service | multi-service Node (web + worker + DB) |
| 46 | [NetBox](https://github.com/netbox-community/netbox) | `9bcfd73994b7` | `fe6f0d311d176ae8…` | Apache-2.0 | Python | multi-service | multi-service Django app |
| 47 | [BookStack](https://github.com/BookStackApp/BookStack) | `a09aa8aac65e` | `85ec64bd57f42b10…` | MIT | PHP | php-service | PHP web app with DB |
| 48 | [Firefly III](https://github.com/firefly-iii/firefly-iii) | `7879e01a0f6a` | `36d3097442e50cdb…` | AGPL-3.0 | PHP | php-service | PHP web app with DB |
| 49 | [OpenRefine](https://github.com/OpenRefine/OpenRefine) | `cb94b3864b8c` | `fa15532cf868c652…` | BSD-3-Clause | Java | jvm-tool | JVM web tool |
| 50 | [Jellyfin](https://github.com/jellyfin/jellyfin) | `96bca6f0bdde` | `3ab226a042d1666a…` | GPL-2.0 | C# | dotnet-service | compiled .NET server |

Expected shapes come from prior knowledge of each project, not Formation
output. Full refs, archive/license hashes, framework family and rationale are
in [JSON](formation-coverage-50-plan.json). Distribution (new 30): static 2,
Vite/static frontend 3, workspace frontend 1, Node service 3, Node monorepo 1,
Python simple 1, Python web 2, Python AI 2, Go 5, Rust 2, multi-service 4,
PHP 2, JVM 1, .NET 1. Quotas were not forced.
