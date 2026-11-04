# Local lifecycle

Status: Current technical reference.

General CLI installation and quickstart are owned by the
[ato.run CLI quickstart](https://ato.run/docs/cli-quickstart/). This page
documents the computation-root wire and profile lifecycle itself.

This page describes the computation-root CLI lifecycle and its local repository.
For contract-root `pack` / `form` / `app` workflows use the [Formation guide](guides/formation.md).
`reform` is also a contract-root command; see [Verify a Derivation transformation](guides/derivation-transformation.md).

The computation-root CLI authors a local computation repository and transports selected
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

For computation-root v2, `encap` is the portable Materialization boundary. `run` verifies the bundle,
selects a compatible restore-capable Materialization, resolves Bindings, and
starts an ephemeral Run without advancing an authored branch.

`reform` does not operate on this computation-root lifecycle. It builds a
candidate Derivation from a new source directory under an existing portable
`.capsule`'s frozen Contract K (contract-root), then verifies it through the
same portable attempt and receipt as `run`. The input bundle and its receipts
are never modified, and nothing is published or activated. That workflow is
documented in [Verify a Derivation transformation](guides/derivation-transformation.md);
it is not part of the command list above.

There are no current lifecycle commands for `lock`, `decap`, or `snapshot`.
`ato run .` and `ato run github.com/owner/repository` belong to the superseded
repository-execution model and are not accepted inputs.

See the accepted [Capsule CLI Lifecycle](rfcs/accepted/CAPSULE_CLI_LIFECYCLE.md).
