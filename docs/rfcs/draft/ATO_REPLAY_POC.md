---
title: "Ato Replay PoC: deterministic Static Web input replay"
status: draft
date: "2026-08-11"
---

# Ato Replay PoC

## Decision

The first replay experiment targets only the staging Static Web delivery of
`gabrielecirulli/2048` on desktop browsers. ato-pwa owns the recorder, player,
microsecond timeline, rAF scheduler, local persistence, validation, and UI.
The application source is unchanged.

The Static Web extractor may, only when
`STATIC_WEB_REPLAY_BRIDGE_ENABLED=true`, copy a transport-only bridge into the
independent extracted output and inject its script tag before application
scripts. The immutable bundle producer remains pure and hashes the resulting
instrumented bytes normally. Unset is false, the reserved bridge path may not
already exist, and the source tree is never modified.

## Replay artifact

`ato.replay/v0` records the immutable `app_url`, selected initial/final
localStorage values, viewport facts, an exact input sequence with relative
microsecond timestamps, and the observed `Math.random` value sequence. The PWA
stores only the latest artifact for the configured capsule, capped at 1 MiB.
Replay refuses an artifact whose `app_url` differs from the current delivery.

The 2048 profile permits only Arrow/WASD/HJKL/R keyboard codes and clicks;
arbitrary text input is never retained. Continuous pointer movement and scroll
are coalesced once per animation frame while discrete causal order is exact.

## Bridge boundary

Bootstrap data is passed through the iframe `name`, capped at 64 KiB, consumed
before application scripts, then cleared. Every message carries an opaque
channel ID. Both sides require the exact iframe window, exact target/parent
origin, and exact channel. The bridge accepts only Ato application origins (or
localhost development), and never receives credentials, cookies, or tokens.

The bridge owns no timeline, artifact storage, scheduler, or UI. It only:

- reads/restores explicitly selected localStorage keys;
- observes or supplies synchronous `Math.random` values;
- reports raw DOM input or dispatches a PWA-scheduled batch;
- reports readiness and final state facts.

## Success condition

Replay is matched only when selected final storage is byte-equivalent and the
recorded random sequence is consumed exactly. Typed divergence reasons are
`state_mismatch`, `random_underflow`, `random_unused`, and `target_mismatch`.

Sharing artifacts through an API, production enablement, touch replay, video,
and pixels are non-goals of v0.
