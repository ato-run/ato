# launch-conditions (local-install fixture)

Deterministic OCI capsule fixture that declares launch conditions so the
installed-state ledger carries non-trivial relaunch claims after
`ato install --from-local`:

- `[state.data] attach = "explicit"` (with a `services.main` state binding) → a
  `state` launch condition recorded with status `UserGrantRequired` (drivable
  later with `ato launch capsule://local/launch-conditions?state.data=prompt`).
- `[targets.app] port = 18891` → a `port` launch condition declaration.

The manifest schema only permits `state_bindings` on OCI targets, so this
fixture is OCI (`busybox`). Packing/launching it needs the image present locally
(Ato performs **no** network pull); seed it once before exercising the launch
path. The install ledger is recorded from the manifest and does **not** need the
image, so the install + ledger path stays hermetic.

It exists primarily to seed the ledger for the future #561 relaunch-condition
smoke; it is not launched in CI (launch needs Desktop/session context + the
image). See `docs/dev-notes/hermetic-desktop-relaunch-smoke.md`.
