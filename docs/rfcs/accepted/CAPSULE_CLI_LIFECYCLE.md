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
sets `main`, and optionally starts a durable Run. `resume` continues the
current head; a historical point requires a new branch. `stop` quiesces,
terminates the owned process tree, records workspace changes, atomically moves
the branch head, and clears active metadata. It creates no portable output.

`encap` is the only portable Materialization boundary. `run` accepts only a
`.capsule` file, imports to temporary storage, restores with a compatible
Materializer, resolves recipient Bindings, and leaves all durable author refs
unchanged.

There are no lifecycle commands or aliases for lock, decap, snapshot, or
repository/Git execution.
