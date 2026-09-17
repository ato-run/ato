# Portable Application Authoring v2

Status: draft implementation contract  
Date: 2026-09-17

## Decision

`schema = "ato.capsule/2"` is a new, strict authoring grammar for compiling a
portable Application directly from a source directory. It does not reinterpret
`ato.capsule/1`, and it does not change any existing v1, wire v2, or wire v3
digest. The compiler produces the existing canonical `BoundContract`,
`ApplicationV2`, `BoundDerivation`, and portable tree objects; it does not add
a second implementation of Contract identity.

An earlier design draft used the integer discriminator `schema = 2`. The
implemented contract uses the namespaced string above so the grammar is
unambiguous beside `ato.capsule/1`. Readers must reject unknown or mismatched
discriminators instead of falling back to either grammar.

## v0 boundary

The supported shape is deliberately bounded:

- one Application Surface on the logical `app.http` Port;
- exactly one immutable `ato.workspace@1` input at `.`;
- one to sixteen explicit process or OCI Derivations;
- one serving step and one HTTP Port per Derivation;
- one or more HTTP GET observations combined with `mode = "all"`;
- optional declared Bindings and at most one declared writable filesystem
  state;
- `effects.default = "pure"`.

This is not a claim of arbitrary-language dependency resolution or a general
distributed Application graph. Process routes require a declared Python
runtime. OCI routes require a digest-pinned image and a supported Linux
platform. Runtime availability remains admission state, not bundle validity.

## Identity

The compiler excludes `capsule.toml` and author-facing Derivation labels from
the immutable workspace and semantic objects. A label is only how a human
selects a route during authoring. Renaming it therefore changes neither K nor
D. Changing argv, runtime constraints, environment, Port, workspace content,
or another realization instruction changes the affected D. Changing an HTTP
expectation changes K.

The output is deterministic. The same source bytes and semantic declarations
produce the same `.capsule` bytes, ContractRef, and sorted DerivationRefs.

## Grammar example

```toml
schema = "ato.capsule/2"

[application]
title = "Example"
surface_path = "/"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[contract]
mode = "all"

[[contract.observation]]
id = "root"
use = "ato.contract.http@1"
port = "app.http"
method = "GET"
path = "/"
status = 200
body_digest = "sha256:0000000000000000000000000000000000000000000000000000000000000000"

[[derivation]]
id = "python-312-a"
use = "ato.process@1"
runtimes = { python = "3.12" }
argv = ["python3", "app.py"]
cwd = "."
env = { MODE = "a" }
guest_port = 8000

[[derivation]]
id = "python-312-b"
use = "ato.process@1"
runtimes = { python = "3.12" }
argv = ["python3", "app.py"]
cwd = "."
env = { MODE = "b" }
guest_port = 8000

[effects]
default = "pure"
```

Unknown fields, duplicate IDs, unsupported protocols, unpinned OCI images,
invalid paths, and invalid digests fail before bundle generation.

## CLI

`ato pack [SOURCE] --output application.capsule` compiles this grammar. The
command reports the transport SHA-256, ContractRef, and every DerivationRef.
Execution remains a separate explicit operation:

```text
ato run application.capsule --derivation sha256:... --no-open
```

When more than one D exists, neither CLI nor Hosted execution may silently
choose the first candidate.
