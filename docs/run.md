# Run

`ato run` compiles a repository into a computation and advances it once:

```text
repository -> adapter -> ato.workspace@1 -> Objects -> Kernel
           -> Workspace Semantics -> Nacelle Provider -> successor ref
```

```bash
ato run .
ato run github.com/owner/repository
ato run . --env MODE=development --secret-ref TOKEN=secret://profile/token
ato run . --allow-network api.example.com
```

Source, exact runtime constraints, entrypoint, semantic environment values,
and filesystem topology contribute to residual identity. Secret values do not;
only safe binding identifiers are stored and the provider resolves values at
realization time. Network and sandbox enforcement are provider concerns.

`ato lock` writes Adapter-owned `capsule.lock`, not a semantic Lock primitive.
`ato run` recomputes its source and baseline ComputationRef before execution;
stale/malformed locks and legacy `ato.lock.json` fail closed. Source bytes are
materialized from Objects into `~/.ato/runs/<run>/workspace-*`; the mutable
authoring repository is never executed.

Secret arguments contain binding identities only. Literal values and the old
`--secret NAME=value` form are rejected. Detached runs with secret bindings
also fail closed until a secure one-shot transport is available. A non-empty
network allowlist runs only on a provider capable of exact enforcement; it is
never widened to full network access.
