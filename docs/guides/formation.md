# Formation guide

Read [K, D and Source](../concepts/application-contracts.md) first. Decide which
observations must survive a new realization, which source identity must be
bound, and which execution route is explicitly authorized. A successful build
alone does not establish that Contract.

## Package explicit portable input

Use `ato pack <source-dir> -o <output.capsule>` for reviewed
[portable authoring v2](../rfcs/draft/PORTABLE_APPLICATION_AUTHORING_V2.md)
input declaring `schema = "ato.capsule/2"`. The
[Getting Started guide](getting-started.md) separates running a supplied static
bundle from packaging the current authored process example. The current
`pack` frontend does not accept the older `ato.capsule/1` source grammar.
Reuse the [authored example](../../samples/portable-multi-process-authored/capsule.toml)
and the v2 input reference rather than mixing fields from historical samples.

Run the resulting bundle with `ato run <bundle> --no-open
--verification-receipt <receipt.json>`. If it declares multiple Ds, supply
`--derivation <declared-sha256-ref>` explicitly. Missing runtimes, incompatible
platforms and absent permissions are admission failures, not proof of bundle
tampering. Never place a secret into an immutable manifest or evidence file.

## Local source Formation

`ato form` is a separate source-candidate workflow. Inspect `ato form --help`
in the same binary you will execute. A local invocation can keep scratch and
output under the checkout:

```sh
mkdir -p .tmp/formation
target/debug/ato form ./my-source --runtime local --network denied \
  --max-attempts 1 --work-root .tmp/formation/work --out .tmp/formation/out
```

This does not install an arbitrary missing toolchain or authorize network
access. Candidate/host support is bounded by the implementation. Network
dependency resolution, browser verification, Runtime Network searches and
managed execution have explicit flags, credentials, containment and budgets;
do not infer permission for them from a local invocation.

## Hosted source and existing bundles

Hosted Formation of `ato.capsule/2` sources uses the
[shared portable builder and attempt](../architecture/hosted-formation-capsule-v2.md).
Source intake, formation, result publication and Hosted Run acceptance are
separate gates. Their presence in code does not prove current deployment or
that all reported staging issues are fixed.

An already formed `.capsule` uses bundle quarantine/validation and Hosted Run
intake. Do not convert it into a source ZIP or form a new K to import it.
Check [Runner / Hosting](runner-hosting.md) and the deployment owner's runbook.

Keep the source closure, selected D, frozen K, attempt outcome and receipt
together. Formation may defer named runtime checks; a claimed accepted Run
requires every required observation to be satisfied. To change an
implementation under an existing K, use [Derivation transformation](derivation-transformation.md).
