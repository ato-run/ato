# Data for foreground portable application Runs

Status: Draft

`ato run <file.capsule|Hub reference>` starts a portable application and opens
the selected Derivation. Computation-root wire v2 keeps its existing
Materializer path. The saved-data lifecycle applies to data that the selected
portable route delivers through declared local filesystem slots or a supported
browser snapshot. It cannot manage external services, arbitrary browser
storage, or data outside the declared boundary.

A new session starts in a private working copy. Reopening an existing state
starts from a separate copy of its most recently saved generation. On stop,
save publishes the working copy as the next generation. Discard deletes the
working copy. It leaves the last saved generation and bundle cache available.
Data deletion makes no claim about overwriting bytes on physical media.
If a local copy cannot be validated or written, the CLI leaves the working
files available for retry or for an explicit discard.

The owner confirms workload shutdown before publishing or deleting data. A
worker stops the workload if the foreground process closes its control pipe.
If the worker itself disappears before it confirms shutdown, the working copy
remains recoverable on disk and is marked as unconfirmed. The CLI never
guesses that resources stopped or removes unconfirmed data. It reports where
to find that data and the Run's evidence.

A terminal offers save, discard, and retain for later. Noninteractive use can
set `--on-exit save` or `--on-exit discard`. Without a policy the working copy
is retained for `ato app recover <DATA_ID> <save|discard>`. An unsuccessful
Run retains recoverable data until explicitly resolved. `--verify-only`
(the existing `--no-open`) stays an ephemeral verification operation. Use
`--headless` for interactive-length Runs that skip opening a browser.

`ato run` shows matching saved states on a terminal. Automation uses
`--data-id <ID>` or explicitly selects `--fresh` when saved or pending states
exist. Continuing a saved state uses the exact immutable bundle revision and
Derivation originally selected. `--fresh` resolves the requested reference
again, including moving Hub tags, and creates independent data. No migration
is inferred. A deleted cache entry is fetched by exact digest or supplied from
the original local file if its digest still matches.

`ato app storage` shows Run data and bundle cache usage. `ato app data-remove`
and `ato app cache-clear` operate on separate storage. Legacy `ato app install`
and its Instance lifecycle remain available with their existing syntax and
behavior. No deprecation or removal is scheduled by this change.

Saved state, its ID, the selected bundle version, and active ownership are
local orchestration. They do not change immutable source Contract or
Derivation identity. Snapshot-bearing transports are handled through the
existing snapshot admission and save path.
