# Run

`ato run` compiles a repository into a computation and advances it once:

```text
repository -> adapter -> ato.workspace@1 -> Objects -> Kernel
           -> Workspace Semantics -> Nacelle Provider -> successor ref
```

```bash
ato run .
ato run github.com/owner/repository
ato run . --env MODE=development --secret TOKEN=vault:project/token
ato run . --allow-network api.example.com
```

Source, exact runtime constraints, entrypoint, semantic environment values,
and filesystem topology contribute to residual identity. Secret values do not;
only safe binding identifiers are stored and the provider resolves values at
realization time. Network and sandbox enforcement are provider concerns.

`ato lock` writes resolution evidence/cache, not a semantic Lock primitive.
Identity-bearing resolved choices are already present in the computation.
