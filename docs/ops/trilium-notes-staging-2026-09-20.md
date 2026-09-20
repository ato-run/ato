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

## Open gaps

**The Instance Surface does not sustain a WebSocket.** Trilium opens one for
live updates. The upgrade does reach Trilium — its log shows repeated
`websocket client connected` — but the socket is torn down within about a
second, and the browser sees an error at ~1.0 s every time. The Runner's own
surface proxy is a raw TCP splice with no timeouts and Caddy passes upgrades,
so the drop is above the Runner.

Consequence for a person using Trilium: the note tree does not refresh by
itself. A note created or renamed appears after a page reload. Editing,
saving, search and persistence are unaffected.

This is a generic capability, not a Trilium feature: any app with live
updates, HMR, collaborative editing, a terminal or streaming chat needs it.

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
