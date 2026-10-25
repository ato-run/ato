# Static Web instance state lane — staging builder record

What is deployed, what it produces, and how to undo it.

This is **not** a builder modernization. It adds artifact-native static state
instrumentation to the builder that was already running. The builder daemon
itself remains a legacy deploy component — see the Legacy Formation Adapter
ADR for its provenance and cutover conditions.

## Deployed binary (staging, `ubuntu-sugamo`)

| | |
|---|---|
| Path | `/usr/local/bin/ato-snapshot-builder` |
| SHA-256 | `a2036feaf44a6cd4ca5c67dc776b0fbb113f222e0e8668118647fcf359420fc1` |
| Source commit | `ec9dba88a15e33bd219df5f8cf09fc42f74dc970` |
| Source branch | `feat/builder-instance-state-artifact-native` (base `deploy/replay-static-lane`) |
| Build | `cargo build --release --bin snapshot-builder` in `~/src/ato-builder-p0` |
| Deployed | 2026-09-02 14:24 UTC |
| Unit | `ato-snapshot-builder.service` |
| Drop-in | `/etc/systemd/system/ato-snapshot-builder.service.d/21-instance-state-bridge.conf` |
| Flag | `STATIC_WEB_INSTANCE_STATE_BRIDGE_ENABLED=true` |

### Rollback

| | |
|---|---|
| Path | `/usr/local/bin/ato-snapshot-builder.pre-p0-20260902` |
| SHA-256 | `c40766bcff225c6b99b5e36368f8d682b81370bff41d6495b95d33b49233d2c3` |
| Built | 2026-08-20 |

```sh
sudo install -m 0755 /usr/local/bin/ato-snapshot-builder.pre-p0-20260902 \
                     /usr/local/bin/ato-snapshot-builder
sudo rm /etc/systemd/system/ato-snapshot-builder.service.d/21-instance-state-bridge.conf
sudo systemctl daemon-reload && sudo systemctl restart ato-snapshot-builder
```

Rolling back does not invalidate artifacts already built with the lane: the
delivery edge keeps its fallback, so both generations serve.

## What the artifact gains

Injected ahead of every application script, into the materialized copy only:

```html
<script id="__ato_instance_state_v1" type="application/json">null</script>
<script src="/__ato/instance-state-bridge-v1.js"></script>
```

`null` is the placeholder. The same immutable bytes are served on the
anonymous public Static Web lane where no ComputeInstance exists, so the
bridge stays inert until an edge that resolved an owner replaces that text.

The delivery edge then rewrites **only that element's text**. It no longer
splices structure into a document it did not build.

## Bridge cache policy — the version is in the path

`/__ato/instance-state-bridge-v1.js` is served `private, max-age=3600`, and
artifacts reference it **by path**. So:

> **Never change what `…-v1.js` returns. To change the bridge, publish
> `…-v2.js` and inject that path from the builder.**

A content change under the same path reaches clients only as their hour-old
cache expires, leaving some Apps running the old script against a new
contract. That is silent and version-invisible. During P0 a content change
was made under `v1` before any artifact referenced it; from now on the path
carries the version.

## Known data contamination (staging only, remediated)

The pre-fix bridge patched storage by assignment. `storage.setItem = fn` does
not shadow the prototype method — `Storage`'s named-property setter runs and
writes a real entry — so the bridge's own function source was stored as if the
App had saved it, and the snapshot reconciler committed it into
`InstanceState`:

```
{"key":"clear","value":"function () {\n  rawClear();\n  recordClear();\n }"}
```

Fixed by `Object.defineProperty` (ato#1327). **Production was never
deployed**, so no production state is affected. One staging instance was
cleaned.

### Cleanup for a contaminated instance

Only instances whose state was written by a bridge older than ato#1327 need
this. From the instance's own origin, signed in as its owner:

```js
await fetch('/__ato/instance-state/local-storage', {
  method: 'POST', credentials: 'same-origin',
  headers: { 'content-type': 'application/json' },
  body: JSON.stringify({
    protocol: 'ato.browser-instance-state@1',
    operations: [
      { kind: 'remove', key: 'setItem' },
      { kind: 'remove', key: 'removeItem' },
      { kind: 'remove', key: 'clear' },
    ],
  }),
});
```

Verify by reloading: the App's real keys remain, those three are gone. Use
targeted removes, never `reset-state` — the latter destroys the App's data
along with the contamination.

To find candidates:

```sh
npx wrangler d1 execute <db> --remote --env staging --command \
  "SELECT instance_id FROM instance_states WHERE state_json LIKE '%rawClear()%'"
```
