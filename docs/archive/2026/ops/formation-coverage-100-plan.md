# Formation 6b-E — current-main 100-app preregistration

**Preregistered; no current-wave app measurement at this commit.** No capability change.

Ato `e494e9375cf1151fc2884d545047dfbdad52c2cc`; API `dcc3049a69051ed6d1c7819580ed1e171815c34c`. Both live main pins matched the expected values. #1449 merge is the Ato pin. Historical `c4c285f3` results and the original 50 source/license identities remain unchanged.

One Linux/aarch64 local Runtime, Ubuntu 24.04.4 LTS, rustc/Cargo 1.96.0, bwrap+landlock, /opt/ato/toolchains. Freshly built fixed binaries; full SHA256 and host facts are in the JSON. Disk after build: 128611356672 bytes free; measurement requires at least 64 GiB. No cleanup of old evidence/state was needed.

100 actual Formation invocations: existing 50 remeasured plus 50 frozen additions. CandidateProducer/DecisionProvider/browser model verifier OFF, model calls 0, network denied, max 4 attempts, hard timeout 900 s, default source/build limits. No per-app authoring, override, explicit source-to-OCI request, source rewrite, or permission expansion. Workload policy is distinct from pre-execution public source/license acquisition.

A source normalized; B K; C D; D admitted/start evidence; E actual realization; F actual Rust Verifier; G fresh fully_satisfied same-K/D receipt; H retained ref plus physical hash inventory. I is unmeasured in this wave. An HTTP 200 is not functional acceptance. The earlier 2048/reveal.js append stays separate.

Primary classes and their precedence are fixed in [classify-100.py](../../scripts/acceptance/coverage/classify-100.py). CandidateProducer-solvable remains a separate false/unknown/true hypothesis, never a primary class. Source errors map to adapter; network_denied maps to effect/policy. All raw errors/timeouts are retained and explained, never silently converted to success. Infra reruns require independent confirmation; app-terminal reruns are forbidden.

Historical comparison records terminal, K/D refs, execution, receipt and retention; unchanged / diagnostic_refinement / reach_improvement / regression / newly_verified. Findings do not trigger repairs in this wave. No deploy, migration, credential reread or #1421 change. Future model reservation 4,513,388 USD micros is unused.

## Source/license replacements before measurement

- #64 Planka (`627701dda3459fa25830b0fd5bb2f5668d033284`) → [wekan](https://github.com/wekan/wekan): PLANKA Community/Fair Use restrictions and Pro/Enterprise exclusions; not an all-OSS full archive. Original frozen archive/license hashes retained in JSON; never executed.
- #82 Meilisearch (`a85bd66e9d7ce30e8e33a17db54b3a2de612e086`) → [sonic](https://github.com/valeriansaliou/sonic): root SPDX MIT AND BUSL-1.1, restricted Enterprise Edition source in the frozen full archive. Original frozen archive/license hashes retained in JSON; never executed.
- #83 Tabby (`21b29048d7bcf6b94f9f482f2d0fd05efadfd19f`) → [rustdesk-server](https://github.com/rustdesk/rustdesk-server): ee/LICENSE has proprietary Enterprise restrictions; frozen full archive is not wholly Apache-2.0. Original frozen archive/license hashes retained in JSON; never executed.
- #95 ThingsBoard (`ae9c7c052858a496d918b2062c90db5461599b65`) → [nifi](https://github.com/apache/nifi): Business Source License 1.1 with license-key and production-size conditions; not OSS at this pin. Original frozen archive/license hashes retained in JSON; never executed.
- #53 `Q42/0hh1` → `florisluiten/0hh1`: original URL returns GitHub API 404; public original-source repository located before any Formation execution.
- #54 `Q42/0hn0` → `Techdojo/0hn0`: original URL returns GitHub API 404; public original-source repository located before any Formation execution.
- #56 `jerosoler/Piano` → `pkalogiros/AudioMass`: original URL and initial AudioMass owner spelling return GitHub API 404; replace with a browser audio editor at its verified public repository before any Formation execution.

GitHub canonically redirects `lynn/hello-wordl` to `lynn/hello`; the returned repo/ref is fixed. Shapes are source hypotheses, not observed reach. GitHub primary language can differ from runtime family (e.g. AdGuard TypeScript source volume, Go service hypothesis). SPDX family identifiers such as GPL-3.0 retain the GitHub legacy identifier; root license bytes/hash are the evidence.

## Additional 50

| # | App / repository | Branch | Exact ref prefix | License | Language | Shape | Monorepo |
|---|---|---|---|---|---|---|---|
| 51 | [A Dark Room](https://github.com/doublespeakgames/adarkroom) | `main` | `1fada4620b6c` | MPL-2.0 | JavaScript | plain-static | no |
| 52 | [Hextris](https://github.com/Hextris/hextris) | `gh-pages` | `3f4847dc8fd7` | GPL-3.0 | JavaScript | plain-static | no |
| 53 | [0h h1](https://github.com/florisluiten/0hh1) | `master` | `25910e580d6e` | MIT | JavaScript | plain-static | no |
| 54 | [0h n0](https://github.com/Techdojo/0hn0) | `master` | `a207bafd9260` | MIT | JavaScript | plain-static | no |
| 55 | [Emoji search](https://github.com/muan/emoji) | `gh-pages` | `21d467d6b3ac` | MIT | CSS | plain-static | no |
| 56 | [AudioMass](https://github.com/pkalogiros/AudioMass) | `production` | `21f5ee1362a4` | MIT | JavaScript | plain-static | no |
| 57 | [SVGOMG](https://github.com/jakearchibald/svgomg) | `main` | `f925656d40a5` | MIT | JavaScript | frontend-spa | no |
| 58 | [quiver](https://github.com/varkor/quiver) | `master` | `2f289ecbae9b` | MIT | JavaScript | frontend-spa | no |
| 59 | [hello wordl](https://github.com/lynn/hello) | `main` | `835db051e78d` | MIT | TypeScript | frontend-spa | no |
| 60 | [diagrams.net](https://github.com/jgraph/drawio) | `dev` | `0f419a92c769` | Apache-2.0 | JavaScript | frontend-spa | yes |
| 61 | [Node-RED](https://github.com/node-red/node-red) | `main` | `1e85f1efbc88` | Apache-2.0 | JavaScript | node-service | yes |
| 62 | [SillyTavern](https://github.com/SillyTavern/SillyTavern) | `release` | `06bde939fb1e` | AGPL-3.0 | JavaScript | node-service | no |
| 63 | [Dockge](https://github.com/louislam/dockge) | `master` | `f809ae192b57` | MIT | TypeScript | node-service | no |
| 64 | [Wekan](https://github.com/wekan/wekan) | `main` | `212a29d62c9c` | MIT | JavaScript | multi-service | yes |
| 65 | [Homepage](https://github.com/gethomepage/homepage) | `dev` | `8d39113bbe7b` | GPL-3.0 | JavaScript | node-service | yes |
| 66 | [Audiobookshelf](https://github.com/advplyr/audiobookshelf) | `master` | `3563d49424d5` | GPL-3.0 | JavaScript | node-service | yes |
| 67 | [Misskey](https://github.com/misskey-dev/misskey) | `develop` | `11692e5ebf44` | AGPL-3.0 | TypeScript | multi-service | yes |
| 68 | [SilverBullet](https://github.com/silverbulletmd/silverbullet) | `main` | `8dbb821e9a2f` | MIT | TypeScript | polyglot-service | yes |
| 69 | [copyparty](https://github.com/9001/copyparty) | `hovudstraum` | `080935788b17` | MIT | Python | python-service | no |
| 70 | [Healthchecks](https://github.com/healthchecks/healthchecks) | `master` | `14008860208e` | BSD-3-Clause | Python | python-service | no |
| 71 | [ArchiveBox](https://github.com/ArchiveBox/ArchiveBox) | `dev` | `09ec9e1f035c` | MIT | Python | python-service | no |
| 72 | [Whoogle](https://github.com/benbusby/whoogle-search) | `main` | `0543f8652867` | MIT | Python | python-service | no |
| 73 | [Superset](https://github.com/apache/superset) | `master` | `6d1bfaa4cd44` | Apache-2.0 | Python | multi-service | yes |
| 74 | [Datasette](https://github.com/simonw/datasette) | `main` | `cec5e6b2ef5d` | Apache-2.0 | Python | python-service | no |
| 75 | [Home Assistant](https://github.com/home-assistant/core) | `dev` | `0dcada8811f3` | Apache-2.0 | Python | python-service | no |
| 76 | [mitmproxy](https://github.com/mitmproxy/mitmproxy) | `main` | `d0d5a70be9bf` | MIT | Python | python-service | no |
| 77 | [Memos](https://github.com/usememos/memos) | `main` | `a80576a6def3` | MIT | Go | go-service | yes |
| 78 | [Gatus](https://github.com/TwiN/gatus) | `master` | `ae605f291928` | Apache-2.0 | Go | go-service | no |
| 79 | [MinIO](https://github.com/minio/minio) | `master` | `7aac2a2c5b7c` | AGPL-3.0 | Go | go-service | yes |
| 80 | [AdGuard Home](https://github.com/AdguardTeam/AdGuardHome) | `master` | `a308a7434284` | GPL-3.0 | TypeScript | go-service | yes |
| 81 | [Beszel](https://github.com/henrygd/beszel) | `main` | `e01e2a20e9d3` | MIT | Go | go-service | yes |
| 82 | [Sonic](https://github.com/valeriansaliou/sonic) | `master` | `a79837d88b91` | MPL-2.0 | Rust | rust-service | yes |
| 83 | [RustDesk Server](https://github.com/rustdesk/rustdesk-server) | `master` | `a7736be5e40f` | AGPL-3.0 | Rust | multi-service | yes |
| 84 | [Atuin](https://github.com/atuinsh/atuin) | `main` | `c8d4d7b5d97c` | MIT | Rust | rust-service | yes |
| 85 | [Redlib](https://github.com/redlib-org/redlib) | `main` | `a4d36e954cf1` | AGPL-3.0 | Rust | rust-service | no |
| 86 | [wallabag](https://github.com/wallabag/wallabag) | `master` | `a5be36bf74b6` | MIT | PHP | php-service | no |
| 87 | [Lychee](https://github.com/LycheeOrg/Lychee) | `master` | `3796a35721b0` | MIT | PHP | php-service | no |
| 88 | [grocy](https://github.com/grocy/grocy) | `master` | `41206cb90154` | MIT | Blade | php-service | no |
| 89 | [FOSSBilling](https://github.com/FOSSBilling/FOSSBilling) | `main` | `668fb8c687e6` | Apache-2.0 | PHP | php-service | yes |
| 90 | [Discourse](https://github.com/discourse/discourse) | `main` | `2590ea9db1b7` | GPL-2.0 | Ruby | multi-service | yes |
| 91 | [Huginn](https://github.com/huginn/huginn) | `master` | `86ac2818e0dc` | MIT | Ruby | ruby-service | no |
| 92 | [Redmine](https://github.com/redmine/redmine) | `master` | `8d04e327e4c5` | GPL-2.0-or-later | Ruby | ruby-service | no |
| 93 | [Jenkins](https://github.com/jenkinsci/jenkins) | `master` | `a2c9bfcc9313` | MIT | Java | jvm-service | no |
| 94 | [Zeppelin](https://github.com/apache/zeppelin) | `master` | `7f7b995c3597` | Apache-2.0 | Java | jvm-service | yes |
| 95 | [NiFi](https://github.com/apache/nifi) | `main` | `26fa6a2587da` | Apache-2.0 | Java | jvm-service | yes |
| 96 | [Guacamole](https://github.com/apache/guacamole-client) | `main` | `38e8cb8ff5cd` | Apache-2.0 | Java | multi-service | yes |
| 97 | [Kavita](https://github.com/Kareadita/Kavita) | `develop` | `1a0ce0051ab6` | GPL-3.0 | C# | dotnet-service | yes |
| 98 | [Jackett](https://github.com/Jackett/Jackett) | `master` | `2a16d0dabd85` | GPL-2.0 | C# | dotnet-service | yes |
| 99 | [Radarr](https://github.com/Radarr/Radarr) | `develop` | `c90668a52066` | GPL-3.0 | C# | dotnet-service | yes |
| 100 | [Sonarr](https://github.com/Sonarr/Sonarr) | `v5-develop` | `a8a82905e7ce` | GPL-3.0 | C# | dotnet-service | yes |

Full commits, archive SHA256, license file SHA256, Dockerfile paths, manifests and layout basis are in [plan JSON](formation-coverage-100-plan.json). Existing 50 identities are copied from the unchanged 20/50 plans; no historical result is added to the new denominator.

Source-to-OCI exists as generic capability (#1446/#1448), not automatic baseline coverage. WBO remains image/artifact PASS with start refused and C/D/E unreached. Vikunja remains unexecuted. Neither contributes a success to this wave.

Completion requires 100/100 actual terminals (all untyped failures explained), A–H funnel, terminal/language/shape distributions, old-50 comparison, regression list, typed-K list and raw evidence hashes. It does not require 100 successes. ROADMAP is updated only after those measurement gates are met.
