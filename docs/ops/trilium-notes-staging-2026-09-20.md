# Trilium Notes staging acceptance — 2026-09-20

Upstream Trilium v0.105.0 as a personal App on staging: imported, run on a
Runner, used as a notes app, and shown to keep its notes across a stop and a
restart.

Production, Discover and Public Try were not touched. The App is private to its
owner.

## Identity

| Field | Value |
| --- | --- |
| Upstream | `TriliumNext/Trilium`, tag `v0.105.0` |
| Commit | `a0908a6e1e1741a3c3824d803da07300183dcb0c` |
| Image | `docker.io/triliumnext/trilium@sha256:1d8492b82e461f9d8cba1acab2bf89d0182821318a1fc1277656e688a6fb4ee4` |
| Image index | `sha256:cc6a15edd7ac3df9bb98ad39dd40af8ffda6c1c0bfafe37c26137ddcbecdb7e5` |
| Licence | AGPL-3.0, from the image's own `org.opencontainers.image.licenses` |

The image's `org.opencontainers.image.revision` equals the tag's commit, so the
release and the image are the same source. Nothing resolves `latest` or a
branch head at run time.

## Bundle

Built with `ato pack samples/trilium-notes`:

| Field | Value |
| --- | --- |
| bundle SHA-256 | `sha256:94f947d451338f4359d11c0575f20c06f40e65a4ff176674ff141507b1bd4806` |
| `ContractRef` | `sha256:f806dcfd5180561f1636706a853af0a54141270caaa72856c9735d2bc3d7b063` |
| `DerivationRef` | `sha256:e9ca1c781fbbe039fb8064717b5d76db21096b6e92b15c93e91be727cdb15ef8` |

## Ato records

| Field | Value |
| --- | --- |
| Instance | `cinst_01M2YH7FDA5Q37911BKY5C10BF` |
| Compute | `cmp_01M2YH7FAEBDB75AWCC96ZX88V` |
| Schema | `csch_01M2YH7FC5CFFX5KJ2C7E0B5WJ` |
| App URL | `https://cinst-tfgnfdkubznb7xh2.stg-app.ato.run/` |
| Realization | OCI container |
| Runner | `ubuntu-sugamo`, worker pid 1067165, slot `127.0.0.1:8420` |
| Availability | `on_demand` ("Starts when opened") |

Imported through the PWA's Add App with the **OCI container** route. The Ready
page reported Contract `Verified` and Surface `Ready`.

## Execution

- start: `/usr/local/bin/node /usr/src/app/main.cjs`
- working directory: the read-only workspace mount (`/app`)
- env: `TRILIUM_DATA_DIR=/home/node/trilium-data`, `TRILIUM_HOST=0.0.0.0`,
  `TRILIUM_PORT=8080`
- state: `/home/node/trilium-data`, read-write, a Runner volume
- readiness / Contract: `GET /` answers 200
- limits: 1 GiB memory, 1000 CPU millis, 256 pids

There is no build step. The upstream image is the artifact; Ato pulls it by
digest and runs it.

## Cold start

From `sleeping` with no container on the Runner (verified with `docker ps` on
the host) to a served page, measured with Navigation Timing in Chrome:

| Metric | Value |
| --- | --- |
| Time to first byte | 4374 ms |
| DOMContentLoaded | 4520 ms |
| Load | 4593 ms |

The image was already present on the Runner, so this excludes the first pull.

## Persistence

Three stop/start cycles, each one an explicit `POST /stop` followed by a
confirmed absence of the container on the host, then a fresh open:

- the note `日本語タイトルの受入テストノート` and its whole body survived every cycle
- Trilium's own login session survived (the session secret is on the volume)
- the app reopened on the same note

## Editor acceptance

Checked in Chrome against the staging App:

| Item | Result |
| --- | --- |
| Japanese text input | pass — title and body |
| Japanese IME composition | not run — the automation entered Unicode text directly and did not drive a live OS IME composition session |
| Enter | pass — new paragraph |
| Shift+Enter | pass — soft line break in the same paragraph |
| Markdown autoformat | pass — `## ` became a heading, `* ` a bullet list, `**…**` bold |
| Heading | pass — rendered and listed in the table of contents |
| Bulleted list | pass — two items |
| Bold | pass |
| Undo / Redo | pass — `cmd+z` removed the last phrase, `cmd+shift+z` restored it |
| Copy / paste | pass — a paragraph copied and pasted as a new paragraph |
| Note switching | pass — root ↔ the note, from the tree |
| Autosave | pass — "保存されました" after each edit, content survived reloads |
| Search | pass — searching `永続化` found the note with a highlighted snippet |

## WebSocket acceptance

The fixed Instance URL now sustains the application WebSocket. The fault was
in the generic process-backed App proxy, not in Trilium, Caddy or the Runner:
`proxyToRuntimeRoute()` rebuilt the upstream `101` response as a new
`Response`. That detached the Cloudflare-runtime-owned WebSocket lifetime and
closed an otherwise healthy connection as soon as the Worker request ended.

The API fix is
[`cd973fac`](https://github.com/ato-run/ato-api/commit/cd973fac5979767c1c9dc309aba99be3f8109546)
([ato-api#665](https://github.com/ato-run/ato-api/pull/665)). It returns an
upstream response with `response.webSocket` unchanged, before applying the
HTTP-only presentation and marker-header pipeline. No Trilium source,
`capsule.toml`, Caddy or Runner change was needed.

The regression test
`passes a process-backed runtime-owned WebSocket response through unchanged`
first failed before the fix because the final response reconstruction rejected
status 101. It now verifies response identity and WebSocket identity as well as
the forwarded path, query, upgrade headers and optional subprotocol. It also
verifies that Ato credentials are stripped while the application's own cookie
is retained.

The fix was deployed to staging as Worker version
`33a4b80f-1d02-4349-a8dc-99ce4c6ceacc`. Acceptance against this Instance found:

| Item | Result |
| --- | --- |
| Browser upgrade | pass — DevTools Network showed `101 Switching Protocols` for `wss://cinst-tfgnfdkubznb7xh2.stg-app.ato.run/` |
| Connection lifetime | pass — 162.7 s observed without a close, then a separate DevTools-captured `101` remained `Pending` for 67.0 s |
| Server side | pass — Trilium logged one `websocket client connected` per page load and did not immediately reconnect in the observation window |
| Create live update | pass — `WebSocket受入 2026-09-20` appeared in the note tree without reload |
| Rename live update | pass — the tree changed to `WebSocket受入 rename済み` without reload |
| Navigation | pass — switched to another note and back while the WebSocket stayed connected |
| HTTP/save/search | pass — save returned success, the renamed note and body were searchable, and normal HTTP navigation continued |
| Persistence | pass — reloading retained `WebSocket受入 rename済み` and `WebSocket live acceptance: 保存とライブ更新を確認。` |

No WebSocket close occurred during either observation, so there was no browser
close code to record. Trilium does not log a close code for this connection.

This is the process-backed fixed-URL invariant used by application WebSockets,
HMR, collaborative editors, streaming/live UIs and WebSocket-backed terminals;
the proxy does not special-case Trilium headers or paths.

## Open gaps

**A personal App carries no category or licence metadata.** The My Apps editor
offers picture, name, colour and letter only. Category and licence exist on the
Discover/Store publication lane, which is a publication and was not done.

**The image's HEALTHCHECK cannot pass.** It runs `su-exec node …`, which needs
root, and the Runner runs the container as the owner of the writable mount.
`docker ps` therefore reports the container unhealthy. Ato does not read that
signal — readiness is the Contract observation — but the status is misleading
to anyone looking at the host.

**"My Data" shows 0 B.** The App detail panel reports "Nothing saved yet" while
the notes are in fact on the Runner volume. That panel describes the State
Revision lane, not a Runner volume, so for this route it says nothing useful.
