# Recipes

## Overview

A recipe is the unit Ato shares, stores, reviews, and runs.

A recipe describes how source inputs become an execution: which source tree or
artifact to use, which tools and runtimes to prepare, what to build, what
services to start, what environment to expose, what filesystem and network
access to allow, and which command to launch.

---

## Source is not recipe

A source repository is raw material. A recipe is the executable interpretation
of that material.

A repository may have zero, one, or many recipes. A recipe may also live outside
the repository and be shared through the Ato Store.

The same source can become different things depending on the recipe:

```text
github.com/org/product
  ├─ web-demo
  ├─ cli-tool
  ├─ desktop-shell
  ├─ local-db-dev
  └─ no-network-demo
```

This is why Ato shares recipes rather than opaque images. Different recipes turn
the same source into different runnable forms.

---

## capsule.toml

`capsule.toml` is the local file format for an Ato recipe.

Despite the file name, it is not a permanent one-to-one identity for a
repository. It is one possible recipe over one or more source inputs.

```text
source inputs
  + recipe (capsule.toml)
  + user environment
  = execution
```

A repository may contain zero, one, or many `capsule.toml` files. A recipe may
also live outside the repository, published through the Store and referenced by
handle.

---

## How a recipe is resolved

When Ato resolves a recipe, it:

1. Identifies source inputs (local directory, Git repository, snapshot, etc.)
2. Selects or infers a recipe for those inputs
3. Projects source inputs into the user's environment
4. Resolves tools, runtimes, dependency services, and policy declared by the recipe
5. Materializes a managed session from the resolved launch graph
6. Records execution identity and a receipt

Each step's output is part of the execution identity. Changing the recipe
changes the execution identity, even if the source is identical.

---

## Multiple recipes for one source

The following patterns are valid:

- One repository with one `capsule.toml` — the common case
- One repository with multiple `capsule.toml` files in different subdirectories
- A community recipe targeting a third-party repository, published separately
- A recipe that references a pinned source snapshot rather than a live branch

Ato does not enforce a one-to-one relationship between repositories and recipes.

---

## Trust model

A recipe is executable configuration. It can define:

- build commands and run commands
- services to start
- environment variable access
- filesystem grants
- network policy
- host bridge capabilities
- state bindings

Treat third-party recipes with the same care you would apply to third-party
source code. A recipe from an unknown author has the same potential reach as
running unknown source code.

When a recipe is loaded from the Store, the Store surface shows:

- recipe publisher
- source reference and compatibility range
- requested permissions (env, filesystem, network, bridge capabilities)
- whether this is a source-author recipe or a community recipe
- verified execution receipts

---

## Specification

- a recipe MUST declare how source inputs are arranged, built, and launched
- `capsule.toml` is the local file format; its schema version follows the UARC spec
- a repository MAY contain zero, one, or many recipes
- a recipe MAY reference source inputs outside its own repository
- recipe identity (recipe snapshot) is part of execution identity
- the Store MUST distinguish source-author recipes from community recipes

References:

- [`rfcs/accepted/CAPSULE_SPEC.md`](rfcs/accepted/CAPSULE_SPEC.md)
- [legacy Capsule Artifact Format v2](rfcs/archived/CAPSULE_FORMAT_V2.md)
- [`rfcs/accepted/CAPSULE_DEPENDENCY_CONTRACTS.md`](rfcs/accepted/CAPSULE_DEPENDENCY_CONTRACTS.md)
- [Execution Identity](execution-identity.md)
- [Capsule](capsule.md)
