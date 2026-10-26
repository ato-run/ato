# Local lifecycle

This page describes the computation-root CLI lifecycle and its local repository.
For contract-root `pack` / `form` / `app` workflows use the [Formation guide](guides/formation.md).

The computation-root CLI authors a local computation repository and transports selected
Capsule points. It does not infer a project from source, install toolchains, or
run Git repositories and URLs.

```text
ato init <capsule> [--initial-only]
ato resume <capsule-selector> [--branch <name>]
ato stop <capsule>
ato encap <capsule-selector> --materialize <id>... -o <file.capsule>
ato run <file.capsule>
ato reform <existing.capsule> <new-source-dir> --out <dir> [--derivation <sha256:...>]
```

`init` reads an explicit `capsule.toml`, seals the initial Computation, creates
`main`, and normally starts a durable Run. `stop` quiesces that Run and advances
the branch head. `resume` continues the current head or forks a historical
Record onto a new branch.

For computation-root v2, `encap` is the portable Materialization boundary. `run` verifies the bundle,
selects a compatible restore-capable Materialization, resolves Bindings, and
starts an ephemeral Run without advancing an authored branch.

`reform` builds a candidate Derivation from a new source directory under an
existing `.capsule`'s frozen Contract K, then verifies it through the same
portable attempt and receipt as `run`. The candidate Derivation is bound
directly to the frozen Contract body: the new source cannot mint or redefine
K, and the output bundle's root Contract object is the frozen canonical bytes.
The candidate manifest may omit `[contract]` (a derivation-candidate
manifest); if it declares one it must canonicalize byte-for-byte to the frozen
K, otherwise the command refuses with `candidate_contract_mismatch` before
building or executing anything. It also refuses before building when K is
source-bound (`ato.contract.workspace@1`). A new `--out` directory receives
`candidate.capsule`, `receipt.json` and a `report.json` sidecar
(`ato.transformation-candidate/1`); the input bundle and its receipts are never
modified, and nothing is published or activated. `ato pack` still requires the
`[contract]` section.

There are no current lifecycle commands for `lock`, `decap`, or `snapshot`.
`ato run .` and `ato run github.com/owner/repository` belong to the superseded
repository-execution model and are not accepted inputs.

See the accepted [Capsule CLI Lifecycle](rfcs/accepted/CAPSULE_CLI_LIFECYCLE.md).
