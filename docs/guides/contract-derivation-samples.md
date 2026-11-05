# Sample Contract and Derivation objects

Status: Current technical reference.

These examples are decoded from committed `.capsule` bundles, not invented. Each
linked JSON file under
[samples/contract-and-derivation](samples/contract-and-derivation/) is the exact
canonical payload used for the reference (no trailing newline is added), so the
SHA-256 of the file equals the `ContractRef` or `DerivationRef` shown.

Verify one file:

```sh
shasum -a 256 docs/guides/samples/contract-and-derivation/interop-static.contract.json
# b565e9d771c53ba03548b1a151c0ebbc2b884c1d6ec532f72596ce33a4b741db
```

The field-level explanation and the authoring input are in
[Contract and derivation formats](contract-and-derivation-format.md).

## 1. Static web with a source-bound Contract

- Bundle: [`samples/interop-static.capsule`](../../samples/interop-static.capsule)
- `ContractRef`: `sha256:b565e9d771c53ba03548b1a151c0ebbc2b884c1d6ec532f72596ce33a4b741db`
- `DerivationRef`: `sha256:87a0ac84ccdf72ce5ffde7ffc12277d60f3097fb277332a48173c4ddadb7a3cb`

### K (interop-static.contract.json)

```json
{
  "requirements": [
    {
      "body_digest": "sha256:5ec8f5a458f7f500116b8394dd550718915f2d960283905210abe03132ee986a",
      "id": "app-proof",
      "method": "GET",
      "path": "/proof.txt",
      "port": "app.http",
      "status": 200,
      "verifier": "ato.contract.http@1"
    },
    {
      "digest": "sha256:923c3f5606851eab36c6e79ddd7582f154b3dcd75e20679c70516328b40d39da",
      "id": "source-identity",
      "input": "workspace",
      "verifier": "ato.contract.workspace@1"
    }
  ],
  "schema": "ato.contract/1"
}
```

### D (interop-static.derivation-static.json)

```json
{
  "effects": "pure",
  "inputs": [
    {
      "content_ref": "sha256:923c3f5606851eab36c6e79ddd7582f154b3dcd75e20679c70516328b40d39da",
      "id": "workspace",
      "protocol": "ato.workspace@1"
    }
  ],
  "ports": [
    {
      "from": "site",
      "id": "app.http",
      "protocol": "ato.http@1"
    }
  ],
  "schema": "ato.derivation/1",
  "state": [],
  "steps": [
    {
      "entry": "index.html",
      "id": "site",
      "op": "serve",
      "protocol": "ato.browser@1",
      "source": "workspace",
      "spa_fallback": false
    }
  ]
}
```

What it shows: one HTTP observation plus a workspace identity observation, so
this K is source-bound, and a static browser `serve` route that exports
`app.http`. What it does not show: a process or OCI route, a scenario, or more
than one derivation.

## 2. One K, two realizations: Python process and OCI container

- Bundle: [`samples/datasette-cpu.capsule`](../../samples/datasette-cpu.capsule)
- `ContractRef`: `sha256:d4bb7e9ae1be0f6b884d0d58561d7f974092de447da3eea9e2ffe5279b004a19`
- Python `DerivationRef`: `sha256:94ab606ee88d41da3af332e72d5da151fd0920a3aadc9c8c94aefaa80518b21f`
- OCI `DerivationRef`: `sha256:d3e7428a6c75e7d5726319ca61f06631e1811a698b2dd16b531029038dfd701f`

### K (datasette-cpu.contract.json)

```json
{
  "requirements": [
    {
      "id": "datasette-entry",
      "method": "GET",
      "path": "/",
      "port": "app.http",
      "status": 200,
      "verifier": "ato.contract.http@1"
    },
    {
      "body_digest": "sha256:8607a7b55d2f0b4f9169a1c63b1554a60a6af1c8424ab591aa9a451265e0ee83",
      "id": "items-in-id-order",
      "method": "GET",
      "path": "/catalog/items.json?_shape=array&_sort=id",
      "port": "app.http",
      "status": 200,
      "verifier": "ato.contract.http@1"
    },
    {
      "body_digest": "sha256:b4d3392b69f97f35ad0c927a0401766b538ebbf1baabd4ee14f620b0ac54632f",
      "id": "quantity-total",
      "method": "GET",
      "path": "/catalog.json?sql=select%20sum(quantity)%20as%20total%20from%20items&_shape=array",
      "port": "app.http",
      "status": 200,
      "verifier": "ato.contract.http@1"
    }
  ],
  "schema": "ato.contract/1"
}
```

### D, Python process (datasette-cpu.derivation-python.json)

```json
{
  "effects": "pure",
  "inputs": [
    {
      "content_ref": "sha256:33e4b47b6a2b3e22121b17823e0512e3c839f26dfef4dddd40b5af3ec71a3c88",
      "id": "workspace",
      "protocol": "ato.workspace@1"
    }
  ],
  "ports": [
    {
      "from": "serve",
      "guest_port": 8000,
      "id": "app.http",
      "protocol": "ato.http@1"
    }
  ],
  "runtimes": {
    "python": "3.12"
  },
  "schema": "ato.derivation/1",
  "state": [],
  "steps": [
    {
      "argv": [
        "python3",
        "bootstrap.py"
      ],
      "cwd": ".",
      "env": {
        "PYTHONDONTWRITEBYTECODE": "1"
      },
      "id": "serve",
      "op": "serve",
      "protocol": "ato.process@1"
    }
  ]
}
```

### D, OCI container (datasette-cpu.derivation-oci.json)

```json
{
  "effects": "pure",
  "inputs": [
    {
      "content_ref": "sha256:33e4b47b6a2b3e22121b17823e0512e3c839f26dfef4dddd40b5af3ec71a3c88",
      "id": "workspace",
      "protocol": "ato.workspace@1"
    }
  ],
  "ports": [
    {
      "from": "serve",
      "guest_port": 8000,
      "id": "app.http",
      "protocol": "ato.http@1"
    }
  ],
  "runtimes": {
    "oci.cpu_limit_millis": "1000",
    "oci.image": "docker.io/datasetteproject/datasette@sha256:0f57db16cf4eb6cca57f1cedaa0a696bca1c65a1d75b8f7ee372c2dd909a32a0",
    "oci.memory_bytes": "268435456",
    "oci.pids_limit": "128",
    "oci.platform": "linux/amd64"
  },
  "schema": "ato.derivation/1",
  "state": [],
  "steps": [
    {
      "argv": [
        "datasette",
        "--immutable",
        "/app/catalog.db",
        "--host",
        "0.0.0.0",
        "--port",
        "8000"
      ],
      "cwd": ".",
      "id": "serve",
      "op": "serve",
      "protocol": "ato.oci@1"
    }
  ]
}
```

Both Ds export the same logical Port `app.http`, and their
`DerivationRef` values differ. This K has three HTTP observations and **no**
workspace requirement, so it is behavioral: the source is not part of K. The
Python route pins a Python runtime and runs `bootstrap.py`; the OCI route pins
an image by digest, platform and CPU/memory/PID limits and runs through
`ato.oci@1`. Neither the image nor the runtime appears in K.

What it shows: the same Contract can be realized by different routes, and the
receipt names the selected `DerivationRef`. What it does not show: that both
routes run on every host. Runtime, image and limits are admission conditions of
each D, not properties of K.

## 3. One source-bound K with static and process routes

- Bundle: [`samples/interop-multi-derivation.capsule`](../../samples/interop-multi-derivation.capsule)
- `ContractRef`: `sha256:94d7bd2900fa01d8b000d1cf095b0f47bfcc279898c830f0542dd53ee99ffdaf`
- Process `DerivationRef`: `sha256:5d1ec4660745130f196102c1bd81828993ab75d69130f0fdf2e1e2598fd9f3cc`
- Static `DerivationRef`: `sha256:5db5b2ba96e84aef2ff2c0e0931e5014ea2e5fb7b400eed8b9b45c40b5755633`

### K (interop-multi-derivation.contract.json)

```json
{
  "requirements": [
    {
      "body_digest": "sha256:0facb416176b5bebf18f709cef9fbc062b1378fdde7675a8bca946e93c9f34d8",
      "id": "app-proof",
      "method": "GET",
      "path": "/proof.txt",
      "port": "app.http",
      "status": 200,
      "verifier": "ato.contract.http@1"
    },
    {
      "id": "app-root",
      "method": "GET",
      "path": "/",
      "port": "app.http",
      "status": 200,
      "verifier": "ato.contract.http@1"
    },
    {
      "digest": "sha256:604c6993af4d6e3b7c2705e663009324d7cdf5e36227db787250369dccba388b",
      "id": "source-identity",
      "input": "workspace",
      "verifier": "ato.contract.workspace@1"
    }
  ],
  "schema": "ato.contract/1"
}
```

### D, process route (interop-multi-derivation.derivation-process.json)

```json
{
  "effects": "pure",
  "inputs": [
    {
      "content_ref": "sha256:604c6993af4d6e3b7c2705e663009324d7cdf5e36227db787250369dccba388b",
      "id": "workspace",
      "protocol": "ato.workspace@1"
    }
  ],
  "ports": [
    {
      "from": "process-site",
      "guest_port": 8000,
      "id": "app.http",
      "protocol": "ato.http@1"
    }
  ],
  "runtimes": {
    "python": "3.12"
  },
  "schema": "ato.derivation/1",
  "state": [],
  "steps": [
    {
      "argv": [
        "python3",
        "-m",
        "http.server",
        "8000",
        "--bind",
        "0.0.0.0",
        "--directory",
        "."
      ],
      "cwd": ".",
      "id": "process-site",
      "op": "serve",
      "protocol": "ato.process@1"
    }
  ]
}
```

### D, static route (interop-multi-derivation.derivation-static.json)

```json
{
  "effects": "pure",
  "inputs": [
    {
      "content_ref": "sha256:604c6993af4d6e3b7c2705e663009324d7cdf5e36227db787250369dccba388b",
      "id": "workspace",
      "protocol": "ato.workspace@1"
    }
  ],
  "ports": [
    {
      "from": "static-site",
      "id": "app.http",
      "protocol": "ato.http@1"
    }
  ],
  "schema": "ato.derivation/1",
  "state": [],
  "steps": [
    {
      "entry": "index.html",
      "id": "static-site",
      "op": "serve",
      "protocol": "ato.browser@1",
      "source": "workspace",
      "spa_fallback": false
    }
  ]
}
```

Both Ds name the same `workspace` content reference, and K pins that content
with `ato.contract.workspace@1`, so this K is source-bound. The process route
serves the tree with `python3 -m http.server`; the static route serves
`index.html` through `ato.browser@1`. A caller selects one declared D and the
receipt records it.

What it shows: several declared realizations for one frozen K, including a
static and a process route over the same source. What it does not show: that D
membership or success is part of K. Adding or failing a route changes
`derivations` in the bundle index, not `ContractRef`.

## 4. Requirement shapes without a committed sample bundle

No committed sample bundle here contains a scenario, TLS or saved-data
requirement. Their canonical requirement shapes, from
[`BoundRequirement`](../../lib/formation/src/authoring.rs), are:

| Verifier | Canonical fields | Notes |
|---|---|---|
| `ato.contract.scenario@1` | `id`, `verifier`, `port` (`app.http` only), `digest` (the `ato.scenario/1` object) | The runtime executes the scenario. Formation defers it because no candidate is running yet. |
| `ato.contract.tls@1` | `id`, `verifier`, `port`, `server_name`, `start` | The certificate is observed at runtime. Formation defers it. Serial and certificate bytes are not part of K. |
| `ato.contract.instance-snapshot@1` | `id`, `verifier`, `digest` | The exact restored saved-data snapshot. |

The authoring grammar for scenario and TLS observations is in
[`ato.capsule/2`](../../lib/formation/src/capsule_toml_v2.rs); a scenario example is in
[Scenario verifier v1](../architecture/scenario-verifier-v1.md).

## 5. Check the samples against the bundles

The references above are also in each bundle index:

```sh
python3 - <<'PY'
import json
for path in ("samples/interop-static.capsule",
             "samples/datasette-cpu.capsule",
             "samples/interop-multi-derivation.capsule"):
    index = json.load(open(path))["index"]
    print(path)
    print("  contract:", index["root_contract_ref"])
    print("  derivations:", *index["derivations"], sep="\n    ")
PY
```

and the file digests match those references:

```sh
shasum -a 256 docs/guides/samples/contract-and-derivation/*.json
```

## See also

- [Contract and derivation formats](contract-and-derivation-format.md): authoring
  input, canonical order, field mapping, guarantees and limits.
- [Capsule bundle overview](../capsule.md) and the
  [wire v3 draft](../rfcs/draft/PORTABLE_APPLICATION_BUNDLE_V3.md).
- [Research tour](research-tour.md) for how these samples fit the wider claim.
