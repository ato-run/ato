# basic-web (local-install fixture)

Deterministic `source/node` capsule fixture for the hermetic
`ato install --from-local` path. Serves a known HTTP response using only Node's
built-in `http` module:

- no external npm dependencies
- no database
- no secrets
- no external network service
- fixed manifest port (`18890`) with `PORT` override support

Used by `ato install --from-local tests/fixtures/local-install/basic-web` to
create a real installed app inside a hermetic `ATO_HOME`, then relaunch it with
`ato launch <ipk>` / `ato launch capsule://local/basic-web`.

See `docs/dev-notes/hermetic-desktop-relaunch-smoke.md` for the full
install → start-desktop → relaunch smoke sequence.
