---
title: "Authoring Source Overlay and Manual Recipe"
status: draft
date: 2026-07-29
author: "@egamikohsuke"
ssot:
  - "crates/snapshot-builder/src/authoring_runtime.rs"
  - "crates/snapshot-builder/src/main.rs"
related:
  - "GITHUB_CAPSULE_REQUEST_PIPELINE.md"
  - "../accepted/CAPSULE_V1_EXECUTION_MODEL_SPEC.md"
---

# Authoring Source Overlay and Manual Recipe

## 1. Purpose

Source-first Authoring may infer a Program Intent, but inference is not the
only producer. An author must also be able to:

1. enter an exact launch argv and have Ato generate `capsule.toml`; and
2. edit the generated `capsule.toml` without committing it to the upstream
   repository.

Both paths use a versioned Source Overlay owned by Ato. They do not create a
fork and do not change the immutable upstream Source Revision.

## 2. Boundaries

The upstream repository URL and exact commit remain the provenance root. A
Source Overlay is subordinate to one `source_revision_id` and is rejected when
presented with another revision.

The first supported overlay schema is `ato.source-overlay/v1`:

- `kind = capsule_toml` stores the exact edited TOML bytes.
- `kind = manual_command` stores exact launch argv, HTTP port, and readiness
  path. The current Capsule v1 surface synthesizes only the root (`/`) HTTP
  probe, so other paths fail closed until readiness path becomes
  identity-bearing in that schema.

Manual argv is an array, never a shell string. The PWA uses one input line per
argument so quoting and word boundaries are not reinterpreted.

## 3. Materialization

The builder downloads and verifies the immutable source archive before reading
the overlay. It then:

1. creates a fresh inference workspace;
2. validates that the overlay names the claimed Source Revision;
3. parses edited TOML as strict Capsule v1, or normalizes the manual argv into
   Program Intent;
4. materializes generated `capsule.toml` only in the builder overlay;
5. resolves and builds in the existing pinned-source lane; and
6. launches the result in the isolated setup runtime.

Edited TOML bytes are preserved for Clean Replay. A manual-command overlay is
rendered deterministically from the normalized Program Intent.

The current interactive-capture subset still rejects authored build steps,
bindings, state, and other unsupported Capsule v1 facets. Manual authoring does
not weaken that gate.

## 4. SetupJournal evidence

After successful boot and readiness, the builder appends lease-authenticated,
monotonically sequenced observations:

- `process_observation` with the exact launched argv and cwd; and
- `surface_observation` with protocol, port, and readiness path.

These observations are persisted by Ato and are the evidence that the command
used to generate the recipe was actually launched. Terminal text remains
diagnostic only.

Every new Setup Session starts its sequence after the Authoring Session's last
persisted SetupJournal event. Duplicate, stale, or replayed sequences are
rejected.

## 5. Editing and stale evidence

Replacing a Source Overlay is allowed only before Clean Replay and only after
the current setup runtime has stopped. Saving an overlay:

- supersedes the current Program Intent;
- clears the current Program Intent and Resolution Lock pointers;
- moves the Authoring Session back to `configuring`; and
- requires a fresh setup build.

Consequently, a Clean Replay receipt from the previous TOML or command cannot
be used for the edited recipe.

## 6. Failure reporting

A setup failure is terminal for that Setup Session. The builder reports a
bounded typed failure with stage, stable error code, and sanitized diagnostic
message under the same authenticated lease. The API persists it as a
`builder_failure` SetupJournal event, marks the setup `failed`, and consumes the
lease. A new attempt receives a new lease and fresh workspace.
