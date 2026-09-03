# instance-state-hydration-v1

The exact bytes the Static Web **State lane** carries end to end.

`canonical.json` is `ato.materialize.browser@1`'s `BrowserStateV1`, JCS-encoded
by `ato-materializer-browser::encode_state`. The same document is what the
delivery edge writes into the entry HTML's `__ato_instance_state_v1` element
and what `assets/instance-state-bridge-v1.js` hydrates `localStorage` from.

Two properties are asserted against this fixture:

1. `state_fixture_is_canonical_browser_materialization_v1` (this crate) —
   re-encoding the fixture through the Browser Materializer reproduces these
   bytes exactly, so the artifact's bridge and the Materializer cannot drift
   into two different state formats.
2. `bridge_asset_matches_the_state_contract` (this crate) — the shipped bridge
   asset reads the same element id, version and field names.

Entries are sorted by key and unique; that ordering is part of the JCS
canonical form, not a presentation choice.
