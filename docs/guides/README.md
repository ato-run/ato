# Developer guides

Status: Current technical reference.

These are repository developer guides: they describe building, verifying,
operating and contributing to this repository. General user documentation —
trying apps, the Library, CLI installation and the user-level concepts — is
owned by the [ato.run documentation](https://ato.run/docs/); see the
[documentation map](../README.md) ownership rules.

- [Getting started](getting-started.md): build the CLI, verify the supplied static bundle,
  inspect its receipt and package current authoring input.
- [Formation](formation.md): choose authoring/formation input, preserve K and
  interpret the result.
- [Derivation transformation](derivation-transformation.md): verify a changed
  implementation against a frozen Contract.
- [Computation lifecycle](../run.md): init, stop, resume and export a local
  computation branch.
- [Runner and Hosting](runner-hosting.md): select local execution, a Connected
  Runner or the hosted product path.
- [Connect a user-owned Runner](connected-runner-self-host.md): enrollment,
  credentials and supervision.
- [Contributing](../../CONTRIBUTING.md): source boundaries, specs, verification
  and independent PRs.

Component-specific instructions stay with their source, including
[Nacelle](../../extensions/providers/nacelle/CONTRIBUTING.md),
[Desktop](../../apps/desktop/README.md), and
[manual tests](../../tests/manual/README.md).
