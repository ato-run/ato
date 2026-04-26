# ADR: Sandboxed One-Shot CLI Contract

- Status: Proposed
- Date: 2026-04-01
- Decision Makers: ato-cli maintainers
- Related: [docs/current-spec.md](docs/current-spec.md), [docs/adr-ato-lock-json-canonical-input.md](docs/adr-ato-lock-json-canonical-input.md), [docs/adr-source-inference-model-for-run-init.md](docs/adr-source-inference-model-for-run-init.md), [docs/architecture-overview.md](docs/architecture-overview.md)

## 1. Context

ato-cli already accepts trailing target arguments for `ato run`, and internal execution planning already models those arguments as an execution override rather than as Ato-owned options.

At the same time, current user-facing behavior is still incomplete for sandboxed one-shot CLI use cases.

The current gaps are not limited to `argv` transport.

- sandboxed source execution can lose target arguments on the native sandbox path
- single-file script execution materializes a temporary workspace, which risks exposing implementation details as if they were part of the public path contract
- file-oriented CLI tools such as document converters require a clear sandbox filesystem contract, not only an argument-passthrough contract
- current runtime and logging language is still biased toward service startup and readiness, even for one-shot command execution

The motivating example is a tool such as `markitdown` executed as a single-file Python script or as a future exported CLI package. The user expectation is straightforward.

- the target should receive the same `argv` values the user typed after `--`
- relative path arguments should keep their meaning relative to the caller's working directory
- sandbox execution should expose only the host paths explicitly granted by the caller
- command exit status should be the primary result for one-shot CLI execution

This ADR defines the public contract for sandboxed one-shot CLI execution in Ato 0.5.x and later.

## 2. Decision

ato-cli defines a first-class execution contract for sandboxed one-shot CLI targets.

The contract has five parts.

1. explicit target resolution
2. canonical target argument syntax
3. stable path semantics for target-visible `argv`
4. explicit host path grants for sandboxed filesystem access
5. job-oriented execution semantics separate from service-oriented readiness semantics

This ADR fixes the public contract. It does not require every implementation detail to be finalized before adoption, but it does require that behavior converge toward this contract and that known mismatches be treated as bugs rather than as alternative semantics.

## 3. Canonical Syntax

### 3.1 Run syntax

The canonical syntax for target arguments is:

```text
ato run <target> -- <target-args...>
```

The canonical syntax for sandboxed one-shot CLI execution is:

```text
ato run --sandbox [sandbox-options...] <target> -- <target-args...>
```

### 3.2 Sandbox options

The public sandbox filesystem options are:

- `--read <path>`
- `--write <path>`
- `--read-write <path>`

`--mount-cwd` may exist as an experimental flag or future extension, but it is not part of the stable 0.5.x contract.

### 3.3 Examples

```text
ato run @publisher/markitdown -- thesis.pdf -o thesis.md
```

```text
ato run --sandbox \
  --read ./reference/phd-thesis.pdf \
  --write ./reference/phd-thesis.md \
  ./scripts/markitdown.py -- \
  ./reference/phd-thesis.pdf -o ./reference/phd-thesis.md
```

## 4. Target Resolution

Target resolution remains fail-closed.

Accepted target classes are:

- local paths
- GitHub shorthand such as `github.com/owner/repo`
- provider-backed targets in canonical `provider:ref` form
- Ato registry references

For remote registry references, `@publisher/slug` is the canonical public syntax.

`publisher/slug` may be accepted as a backward-compatible alias, but docs, examples, and error guidance should standardize on `@publisher/slug`.

The following are not part of this ADR and must not be introduced as implicit resolution behavior.

- bare remote lookup such as `ato run markitdown`
- ecosystem-specific slash shorthand such as `pip/markitdown`
- slug-only remote disambiguation at execution time

## 5. Target Argument Contract

### 5.1 Passthrough

Everything after `--` is target-owned `argv`.

Ato must preserve those arguments as a sequence of strings.

- Ato must not reinterpret target arguments as Ato options
- Ato must not concatenate target arguments into a single shell string
- Ato must not use shell re-parsing such as `sh -c` to reconstruct target arguments
- Ato must not perform additional quote, glob, or variable expansion on target arguments

### 5.2 Stable target-visible paths

If a target argument contains a path string, that path string must remain directly usable by the target.

This means:

- target `argv` path strings must be usable as passed
- Ato must not rewrite path strings inside target `argv` to different visible paths
- Ato may not substitute a target-visible temporary path for a caller-visible path argument

This rule applies even if the implementation internally materializes temporary workspaces, bridge manifests, staging directories, or ephemeral runtime trees.

### 5.3 Default target args

If the selected target has default runtime arguments, Ato may append user-provided target arguments after those defaults, but the user-provided argument strings themselves remain opaque and must not be rewritten.

## 6. Path Semantics And Working Directory

### 6.1 Caller cwd semantics

Relative path arguments are interpreted relative to the caller's working directory unless the command contract explicitly defines another cwd.

For sandboxed one-shot CLI execution, this caller-visible cwd semantics is part of the public contract.

### 6.2 Materialization is an implementation detail

Single-file script support may materialize a temporary workspace. That materialization is an implementation detail only.

It must not change the meaning of target `argv` path strings.

It is acceptable for internal files such as `main.py` or generated lockfiles to live under temporary directories. It is not acceptable for those temporary paths to replace caller-visible `argv` paths.

### 6.3 Optional future cwd controls

An explicit `--cwd <path>` flag may be added in the future, but this ADR does not require it for the initial stable contract.

## 7. Sandboxed Filesystem Contract

### 7.1 Default visibility

Under `--sandbox`, host filesystem access is deny-by-default.

The target may access only the host paths explicitly granted by the caller, plus implementation-defined temporary sandbox storage such as `/tmp`.

### 7.2 Grant kinds

`--read <path>` grants read-only access.

`--write <path>` grants create and update permission for that exact file path or for entries under that granted directory path.

`--read-write <path>` grants both read and write access.

These options must accept both files and directories.

### 7.3 `--write <file>` semantics

When the granted path is a file path, `--write <file>` means permission to create or update that file path. It is not merely permission to write somewhere under the parent directory by implication.

### 7.4 Stable target-visible paths

The target must observe the same path strings that the caller passed in `argv`.

If a caller grants `./reference/phd-thesis.pdf` and passes that same path in target `argv`, the target must be able to open that same string path successfully, subject to normal application behavior.

### 7.5 No hidden widening

Ato must not widen filesystem grants implicitly beyond the declared scope, except for tightly bounded implementation storage such as `/tmp`.

## 8. Grant Evaluation And Escape Prevention

### 8.1 Normalization

Grant evaluation must be performed against normalized absolute paths.

The same rule applies to best-effort preflight checks that compare requested I/O paths against granted paths.

### 8.2 Symlink escape rejection

If a granted path or a target-accessed path would escape the granted scope through symlink traversal, the access must be rejected.

This rule is fail-closed.

It is not sufficient to compare only the original lexical path strings. Resolution must defend against grant-outside access through symlinks.

### 8.3 No argv rewrite as escape handling

Escape prevention must not be implemented by silently rewriting target `argv` path strings to different internal paths.

If access is unsafe or outside grant, Ato must deny the access or fail preflight. It must not change the target-visible path contract to compensate.

## 9. Preflight And I/O Inference

### 9.1 Best-effort only

Preflight inference of file I/O from `argv` is best-effort only.

It is useful for early feedback, but completeness is not guaranteed.

This means:

- Ato may detect obvious missing read or write grants from known argument patterns
- Ato may suggest corrective flags before launch
- Ato must not claim that preflight can detect every possible runtime file access

### 9.2 Expected behavior

If a clearly inferable input path lacks a read grant, Ato should fail early with a targeted message.

If a clearly inferable output path lacks a write grant, Ato should fail early with a targeted message.

If access cannot be inferred preflight but is denied at runtime, the sandbox must still fail closed.

### 9.3 Example diagnostics

```text
Missing read grant for ./reference/phd-thesis.pdf

Try:
  --read ./reference/phd-thesis.pdf
```

```text
Missing write grant for ./reference/phd-thesis.md

Try:
  --write ./reference/phd-thesis.md
```

## 10. Single-File Script Positioning

Single-file script execution is not a separate public execution model.

It is a source-target convenience entry that compiles into the same execution contract as other source-backed targets.

Therefore, single-file targets must obey the same rules.

- same target resolution model
- same `argv` passthrough model
- same caller-visible path semantics
- same sandbox filesystem grant model
- same job-versus-service execution distinction

Any current mismatch between single-file sandbox execution and this contract is a bug.

## 11. Job Versus Service Semantics

### 11.1 Job targets

One-shot CLI execution is a job target.

For job targets:

- exit code is the primary result
- readiness is not part of the user-facing success contract
- missing readiness configuration is normal
- logs and diagnostics should describe the execution as a command or job, not as a service lifecycle

### 11.2 Service targets

Service targets may use readiness probes, background semantics, supervisor coordination, and dependency sequencing.

That service model remains valid, but it must not leak into the public contract for one-shot CLI execution.

### 11.3 Logging rule

For one-shot CLI targets, user-facing errors should avoid service-oriented wording such as `service exited before readiness` unless the command is genuinely running under a service contract.

## 12. Non-Goals

This ADR intentionally does not define:

- bare-name remote package lookup
- provider-specific resolution beyond canonical `provider:ref` parsing and one-shot execution
- automatic full-fidelity discovery of every file access from `argv`
- stable semantics for `--mount-cwd` in 0.5.x
- package metadata hints for automatic I/O grants
- final install-time command exposure or collision policy for exported CLI shims

## 13. Consequences

Adopting this ADR implies the following.

- sandboxed source execution that drops target `argv` is a bug
- implementations that change target-visible path strings are invalid
- temporary materialization paths must stay internal
- sandbox path grants must be checked against normalized absolute paths
- symlink-based grant escape must be denied
- one-shot CLI UX must not be forced through a service-readiness mental model

## 14. Acceptance Criteria

The contract defined by this ADR is considered satisfied only when the following are true.

### 14.1 Argument transport

- `ato run <target> -- <target-args...>` preserves target arguments as `string[]`
- execution plans preserve target-owned argument ordering
- no shell-style argument reconstruction is used

### 14.2 Target-visible path stability

- path strings passed in target `argv` remain directly usable by the target
- Ato does not rewrite target `argv` path strings to different visible paths
- single-file materialization does not change caller-visible path semantics

### 14.3 Sandbox filesystem grants

- `--read`, `--write`, and `--read-write` work for both files and directories
- `--write <file>` allows create and update of that file path
- grant evaluation is performed against normalized absolute paths
- symlink-based escape outside grant scope is rejected fail-closed

### 14.4 Preflight behavior

- best-effort missing-grant diagnostics are emitted for obvious read and write paths
- preflight does not promise complete runtime I/O discovery
- runtime still fails closed when undeclared access occurs

### 14.5 Job semantics

- one-shot CLI execution succeeds without readiness configuration
- exit code is the primary result
- user-facing diagnostics avoid service-oriented wording for job targets
