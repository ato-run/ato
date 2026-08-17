# Local lifecycle

The current CLI authors a local computation repository and transports selected
Capsule points. It does not infer a project from source, install toolchains, or
run Git repositories and URLs.

```text
ato init <capsule> [--initial-only]
ato resume <capsule-selector> [--branch <name>]
ato stop <capsule>
ato encap <capsule-selector> --materialize <id>... -o <file.capsule>
ato run <file.capsule>
```

`init` reads an explicit `capsule.toml`, seals the initial Computation, creates
`main`, and normally starts a durable Run. `stop` quiesces that Run and advances
the branch head. `resume` continues the current head or forks a historical
Record onto a new branch.

`encap` is the portable Materialization boundary. `run` verifies the bundle,
selects a compatible restore-capable Materialization, resolves Bindings, and
starts an ephemeral Run without advancing an authored branch.

There are no current lifecycle commands for `lock`, `decap`, or `snapshot`.
`ato run .` and `ato run github.com/owner/repository` belong to the superseded
repository-execution model and are not accepted inputs.

See the accepted [Capsule CLI Lifecycle](rfcs/accepted/CAPSULE_CLI_LIFECYCLE.md).
