# Capsule CLI Lifecycle

Status: Accepted

The normal public CLI is:

```text
ato init <capsule> [--initial-only]
ato resume <capsule-selector> [--branch <name>]
ato stop <capsule>
ato encap <capsule-selector> --materialize <id>... -o <file.capsule>
ato run <file.capsule>
```

`init` parses explicit authoring configuration, preflights Adapters, seals C0,
sets `main`, and optionally starts a durable Run. A worker publishes `ACTIVE`
only after all configured processes and live Adapter instances have attached
successfully. `resume` continues the
current head; a historical point requires a new branch. `stop` quiesces,
terminates the owned process tree, records workspace changes, atomically moves
the branch head, and clears active metadata. It creates no portable output.

`resume` and portable `run` use the same Materializer and RealizationDriver
reconstruction path. Resume differs only by publishing the reconstructed realization
as a durable local Run. Restoring only the selected workspace and assigning its
ComputationRef as the Run head is forbidden because non-workspace semantic state
would not be realized.

`encap` is the only portable Materialization boundary. `run` accepts only a
`.capsule` file, imports to temporary storage, resolves recipient Bindings,
and delegates all physical restoration to one compatible Materializer. It
must not directly restore the target before that selection, and it leaves all
durable author refs unchanged.

There are no lifecycle commands or aliases for lock, decap, snapshot, or
repository/Git execution.
