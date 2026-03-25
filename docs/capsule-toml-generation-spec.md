# capsule.toml Generation Specification

This document defines the strict, ambiguity-free generation and compatibility contract for capsule.toml.
It is intended for automated generators, especially Store-side GitHub inference and preview manifest generation.

This document is normative.

- MUST, MUST NOT, SHOULD, and MAY are used as defined by RFC 2119.
- When this document conflicts with looser examples elsewhere, this document wins for generated manifests.
- This document distinguishes between canonical output and accepted compatibility input.

## 1. Scope

This document defines:

- the canonical manifest shape that generators SHOULD emit
- the exact meaning of each generated field
- the accepted compatibility inputs that parsers and inference layers MUST accept
- normalization rules when source inference starts from higher-level selectors or shorthand fields such as run
- forbidden constructs that generators MUST NOT emit
- valid concrete examples, including native delivery

This document does not attempt to document every internal field supported by the parser.
It defines the subset that Store-side generators should produce and the compatibility inputs they must understand.

## 2. Schema Version Policy

### 2.1 Canonical output

Generators SHOULD emit schema_version = "1" for newly generated manifests.

Canonical generated profile:

- schema_version MUST be "1"
- type MUST be "app" unless the generator is explicitly producing a library manifest
- default_target MUST be present and MUST point at an existing target label
- generated runnable manifests SHOULD use a single target named app unless there is a concrete reason to emit multiple targets

### 2.2 Accepted compatibility input

The parser and normalization pipeline currently accept these authored forms:

- schema_version = "1": current canonical form
- schema_version = "0.3": compatibility input form
- schema_version = "0.2": deprecated compatibility input form
- omitted schema_version in CHML-like shorthand inputs: compatibility input form

Generators MUST treat schema_version = "0.2" as deprecated.
Generators MUST NOT emit schema_version = "0.2" unless they are intentionally producing a compatibility manifest for a pinned downstream consumer or test.
Generators MUST NOT emit schema_version = "0.3" for normal Store-generated runnable manifests.

Recommended minimal generated shape:

```toml
schema_version = "1"
name = "example-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
driver = "python"
runtime_version = "3.11.10"
entrypoint = "main.py"
```

## 3. Notation and Naming Rules

Generators MUST follow these notation rules:

- TOML strings MUST use double quotes
- integers MUST be decimal integers
- booleans MUST be lowercase true or false
- field names and table names MUST use snake_case
- table names MUST be explicit and MUST NOT rely on implicit dotted-object side effects
- arrays MUST preserve generator-chosen order; the order is semantically meaningful for cmd, required_env, outputs, build_env, finalize.args, and dependency arrays
- generators MUST NOT emit duplicate keys
- generators MUST NOT emit empty-string keys
- generators SHOULD omit absent optional fields instead of emitting empty placeholders

Compatibility note:

- This document standardizes field and table identifiers on snake_case
- Historical hyphenated compatibility names may still appear in legacy docs or artifact-internal metadata, but they are not canonical authored keys
- The manifest name value is a separate rule: the current validator still requires lowercase kebab-case for name

Recommended deterministic top-level field order:

1. schema_version
2. name
3. version
4. type
5. default_target
6. metadata
7. requirements
8. network
9. build
10. pack
11. targets
12. services
13. artifact
14. finalize

Recommended deterministic target field order:

1. runtime
2. driver
3. language
4. runtime_version
5. runtime_tools
6. entrypoint
7. image
8. run
9. run_command
10. cmd
11. env
12. required_env
13. working_dir
14. port
15. readiness_probe
16. package_type
17. build_command
18. outputs
19. build_env
20. package_dependencies
21. external_dependencies
22. external_injection

Canonical output SHOULD prefer entrypoint and run_command over run.
The run field is listed above because it is an accepted compatibility input.

## 4. Top-Level Rules

### 4.1 Required fields

Generated app manifests MUST include:

- schema_version
- name
- version
- type
- default_target
- [targets.<default_target>]

### 4.2 name

- name MUST be lowercase kebab-case under the current validator
- name length MUST be between 3 and 64 characters
- name MUST be stable across retries for the same repository unless the repository identity itself changed

Valid examples:

- hello-capsule
- web-dashboard
- notee-api

Invalid examples:

- HelloCapsule
- hello_capsule
- hi

### 4.3 version

- version MUST be valid semver when present
- generators SHOULD use 0.1.0 when no reliable version can be inferred
- generators MUST NOT emit an empty version string

### 4.4 default_target

- default_target MUST reference an existing [targets.<label>] table
- generators SHOULD use app as the default target label for inferred single-target applications

## 5. Target Rules

### 5.1 Allowed runtime values

Canonical generators MUST emit one of:

- source
- web
- wasm
- oci

Generators MUST NOT emit higher-level selector strings such as these in canonical output:

- web/static
- web/node
- web/deno
- web/python
- source/node
- source/deno
- source/python
- source/native

Those selectors are accepted compatibility inputs, not canonical output.

### 5.2 Allowed driver values

Generators MAY emit only the drivers allowed by the selected runtime:

- runtime = "source": node, deno, python, native
- runtime = "web": static, node, deno, python
- runtime = "wasm": wasmtime
- runtime = "oci": driver SHOULD be omitted unless a future runtime contract explicitly requires one

Generators MUST NOT emit:

- browser_static
- browser-static
- pip
- cargo
- generic shell words as drivers

### 5.3 Path rules

For all generated path-like strings:

- paths MUST be relative to the manifest root unless a field explicitly documents otherwise
- paths MUST NOT be absolute
- paths MUST NOT contain ..
- entrypoint MUST NOT contain shell pipelines or multi-word command strings when it is interpreted as a path
- working_dir MUST be a relative directory path when present

## 6. Launch Surface Rules

Accepted launch surface fields are:

- entrypoint
- run_command
- run
- cmd

Canonical generation rules:

- generators SHOULD emit entrypoint when the runtime directly executes a file or bundle path
- generators SHOULD emit run_command when the runtime must execute a shell-style command or package task
- generators SHOULD emit run only for explicit compatibility output; otherwise they SHOULD normalize it to entrypoint or run_command before writing the final manifest
- generators MUST NOT emit an empty launch field

Compatibility input rules:

- parsers and inference layers MUST accept run as an input field
- for non-static targets, run SHOULD normalize to run_command unless the value is definitively a direct file entrypoint
- for web/static, run MUST normalize to entrypoint as a directory and MUST NOT remain a command string

## 7. Runtime-Specific Generation Rules

### 7.1 source targets

For generated source targets:

- runtime MUST be "source"
- driver MUST be present
- runtime_version MUST be present for node, deno, and python when the generator has enough information to pin a version
- at least one of entrypoint or run_command MUST be present in canonical output
- at least one of entrypoint, run_command, or run MUST be present in accepted input

Use entrypoint when the runtime should directly execute a file.
Use run_command when the project is started through a package script or shell command wrapper.

Examples:

```toml
schema_version = "1"
name = "python-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
driver = "python"
runtime_version = "3.11.10"
entrypoint = "main.py"
```

```toml
schema_version = "1"
name = "node-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
driver = "node"
runtime_version = "20.12.0"
run_command = "npm start"
working_dir = "app"
```

#### 7.1.1 source/native and native delivery

For native app delivery:

- runtime MUST be "source"
- driver MUST be "native"
- source projects MUST declare native delivery metadata in capsule.toml when explicit delivery metadata is required
- source projects MUST NOT author ato.delivery.toml as input
- command-driven native delivery MUST define both [artifact] and [finalize]
- generators MUST NOT emit only one of [artifact] or [finalize]

Simple native app bundle form:

- a .app bundle entrypoint is sufficient for current native delivery detection
- when entrypoint directly names the produced app bundle, ato can currently derive delivery defaults internally

Current derived defaults for the simple .app form:

- artifact.framework = "tauri"
- artifact.stage = "unsigned"
- artifact.target = "darwin/arm64"
- artifact.input = <targets.<default_target>.entrypoint>
- finalize.tool = "codesign"
- finalize.args = ["--deep", "--force", "--sign", "-", <artifact.input>]

Simple native example:

```toml
schema_version = "1"
name = "my-app"
version = "0.1.0"
type = "app"
default_target = "desktop"

[targets.desktop]
runtime = "source"
driver = "native"
entrypoint = "MyApp.app"
```

Command-driven native delivery example:

```toml
schema_version = "1"
name = "time-management-desktop"
version = "0.1.0"
type = "app"
default_target = "desktop"

[targets.desktop]
runtime = "source"
driver = "native"
entrypoint = "sh"
cmd = ["build-app.sh"]
working_dir = "."

[artifact]
framework = "tauri"
stage = "unsigned"
target = "darwin/arm64"
input = "src-tauri/target/release/bundle/macos/time-management-desktop.app"

[finalize]
tool = "codesign"
args = ["--deep", "--force", "--sign", "-", "src-tauri/target/release/bundle/macos/time-management-desktop.app"]
```

### 7.2 web targets

For generated web targets:

- runtime MUST be "web"
- driver MUST be present
- port MUST be present in generated manifests
- public MUST NOT be emitted

#### 7.2.1 web/static

For static web delivery:

- driver MUST be "static"
- entrypoint MUST be present in canonical output
- entrypoint MUST name a directory, not a file
- run_command MUST NOT be emitted

This point is strict and important:

- if the generated site root is the repository root and the repository contains index.html at the root, entrypoint MUST be "."
- if the generated site root is dist and the repository contains dist/index.html, entrypoint MUST be "dist"
- generators MUST NOT emit entrypoint = "index.html"
- generators MUST NOT emit entrypoint = "dist/index.html"
- compatibility input MAY use run = "index.html" or run = "dist/index.html", but canonical output MUST normalize those to directory entrypoint values

Valid examples:

```toml
schema_version = "1"
name = "hello-capsule"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "web"
driver = "static"
entrypoint = "."
port = 8000
```

```toml
schema_version = "1"
name = "vite-static"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "web"
driver = "static"
entrypoint = "dist"
port = 4173
```

Invalid examples:

```toml
[targets.app]
runtime = "web"
driver = "static"
entrypoint = "index.html"
port = 8000
```

```toml
[targets.app]
runtime = "web"
driver = "browser_static"
entrypoint = "dist"
port = 8000
```

#### 7.2.2 web/node, web/deno, web/python

For dynamic web apps:

- runtime MUST be "web"
- driver MUST be node, deno, or python
- port MUST be present
- generators MAY emit entrypoint, run_command, or both depending on the runtime contract
- accepted input MAY also use run
- runtime_version SHOULD be present for deterministic execution

Valid examples:

```toml
schema_version = "1"
name = "web-deno"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "web"
driver = "deno"
runtime_version = "2.1.3"
run_command = "deno task start"
port = 8000
```

```toml
schema_version = "1"
name = "web-node"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "web"
driver = "node"
runtime_version = "20.12.0"
run_command = "npm start"
port = 3000
working_dir = "web"
```

### 7.3 oci targets

For container-backed apps:

- runtime MUST be "oci"
- image SHOULD be emitted when a concrete image reference is known
- entrypoint MAY be emitted when a path-like launcher contract is used instead of image metadata
- if image is emitted, generators SHOULD omit driver

Example:

```toml
schema_version = "1"
name = "oci-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/oci-app:1.0.0"
port = 8080
```

### 7.4 wasm targets

For wasm apps:

- runtime MUST be "wasm"
- driver SHOULD be "wasmtime"
- entrypoint MUST point at the component path when the current runtime path expects one

Example:

```toml
schema_version = "1"
name = "wasm-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "wasm"
driver = "wasmtime"
entrypoint = "dist/app.wasm"
```

## 8. Services Mode

Generators SHOULD emit top-level [services] only when the application is genuinely multi-service.

Current generated services mode rules:

- target runtime MUST be "web"
- target driver MUST be "deno"
- top-level [services.main] MUST exist
- each service MUST define entrypoint or target according to the current service contract
- generators MUST NOT use services mode for web/static

Example:

```toml
schema_version = "1"
name = "multi-service-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "web"
driver = "deno"
runtime_version = "2.1.3"
port = 8000

[services.main]
entrypoint = "services/web.ts"
depends_on = ["api"]

[services.api]
entrypoint = "services/api.ts"
```

## 9. Compatibility Inputs and Normalization Rules

When a generator or inference layer begins from a higher-level inferred runtime selector or shorthand field, it MUST normalize to the canonical target form using these rules.

### 9.1 Runtime selector mapping

- web/static -> runtime = "web", driver = "static"
- web/node -> runtime = "source", driver = "node" in the current normalization path unless the producer is intentionally emitting canonical web runtime output
- web/deno -> runtime = "source", driver = "deno" in the current normalization path unless the producer is intentionally emitting canonical web runtime output
- web/python -> runtime = "source", driver = "python" in the current normalization path unless the producer is intentionally emitting canonical web runtime output
- source/node -> runtime = "source", driver = "node"
- source/deno -> runtime = "source", driver = "deno"
- source/python -> runtime = "source", driver = "python"
- source/native -> runtime = "source", driver = "native"

Store-side generators SHOULD emit canonical runtime = "web" for dynamic web targets they already understand as web workloads.
The source-mapped forms above describe the current compatibility normalization path and must be understood by tooling.

### 9.2 run field mapping

If the input manifest or inference layer starts from a field named run:

- for web/static, map run to entrypoint as a directory
- for all other runtimes, map run to run_command unless the inferred value is definitively a direct file entrypoint

web/static directory conversion rules:

- run = "index.html" -> entrypoint = "."
- run = "dist/index.html" -> entrypoint = "dist"
- run = "public/index.html" -> entrypoint = "public"
- run = "." -> entrypoint = "."
- run = "dist" -> entrypoint = "dist"

Generators MUST NOT preserve run as run_command for web/static.

### 9.3 Deprecated 0.2 input normalization

When the input manifest is schema_version = "0.2":

- the manifest is accepted as deprecated compatibility input
- generators SHOULD upgrade it to schema_version = "1" when rewriting or regenerating the manifest
- deprecated forms SHOULD be normalized into the canonical field set before the manifest is persisted again

## 10. Forbidden Generated Patterns

Generators MUST NOT emit any of the following in normal canonical output:

- schema_version = "0.2"
- schema_version = "0.3"
- runtime = "web/static"
- runtime = "source/node"
- runtime = "source/deno"
- runtime = "source/python"
- runtime = "source/native"
- driver = "browser_static"
- public for runtime = "web"
- entrypoint = "index.html" for web/static
- entrypoint containing shell command strings such as "npm start" or "python app.py"
- missing runtime_version for source/node, source/deno, source/python, web/node, web/deno, or web/python when the generator has enough information to pin a version
- default_target that does not resolve to an emitted target
- absolute paths
- path values containing ..
- source-project ato.delivery.toml authored as a primary source manifest
- [artifact] without [finalize] for native delivery
- [finalize] without [artifact] for native delivery

## 11. Generator Checklist

Before returning a generated manifest, the generator SHOULD verify all of the following:

1. schema_version is "1"
2. name is kebab-case and length-valid under the current validator
3. version is valid semver
4. default_target exists
5. exactly one canonical [targets.<label>] table exists unless multi-target emission is intentional
6. runtime and driver are in the allowed set
7. web targets include port
8. web/static uses a directory entrypoint, never an HTML file path
9. source and dynamic web targets include runtime_version when driver is node, deno, or python and the version is knowable
10. native command-driven targets define both [artifact] and [finalize]
11. run compatibility inputs are normalized before final canonical output unless a compatibility manifest is intentionally being emitted
12. no forbidden fields or deprecated aliases are present

## 12. Shortest Valid Examples by Runtime

### source/python

```toml
schema_version = "1"
name = "py-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
driver = "python"
runtime_version = "3.11.10"
entrypoint = "main.py"
```

### source/node

```toml
schema_version = "1"
name = "node-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
driver = "node"
runtime_version = "20.12.0"
run_command = "npm start"
```

### source/native simple

```toml
schema_version = "1"
name = "desktop-app"
version = "0.1.0"
type = "app"
default_target = "desktop"

[targets.desktop]
runtime = "source"
driver = "native"
entrypoint = "MyApp.app"
```

### source/native command-driven

```toml
schema_version = "1"
name = "desktop-builder"
version = "0.1.0"
type = "app"
default_target = "desktop"

[targets.desktop]
runtime = "source"
driver = "native"
entrypoint = "sh"
cmd = ["build-app.sh"]

[artifact]
framework = "tauri"
stage = "unsigned"
target = "darwin/arm64"
input = "dist/MyApp.app"

[finalize]
tool = "codesign"
args = ["--deep", "--force", "--sign", "-", "dist/MyApp.app"]
```

### web/static at repository root

```toml
schema_version = "1"
name = "static-root"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "web"
driver = "static"
entrypoint = "."
port = 8000
```

### web/static in dist

```toml
schema_version = "1"
name = "static-dist"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "web"
driver = "static"
entrypoint = "dist"
port = 4173
```

### web/deno services mode

```toml
schema_version = "1"
name = "web-services"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "web"
driver = "deno"
runtime_version = "2.1.3"
port = 8000

[services.main]
entrypoint = "services/web.ts"
```

### oci

```toml
schema_version = "1"
name = "oci-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:1.0.0"
port = 8080
```