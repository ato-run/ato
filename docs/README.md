# Ato documentation

Ato advances addressable computations. Use `ato run .` for a local repository
or `ato run github.com/owner/repository` for Git source. A repository adapter
compiles source evidence into `ato.workspace@1`; the kernel persists each
semantic successor as a `ComputationRef`, and Nacelle realizes the physical
process under provider policy.

```bash
ato run .
ato lock .
ato encap . -o app.capsule
ato decap start app.capsule
```

Start with [Run](run.md), [Capsule](capsule.md), and the normative
[Computation Architecture](rfcs/accepted/COMPUTATION_ARCHITECTURE.md).
