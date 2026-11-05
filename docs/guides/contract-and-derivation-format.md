# Contract and derivation formats

Status: Current technical reference.

The [README](../../README.md) describes Contract K and Derivation D as the
conditions and the declared route. This page shows the concrete formats: the
authoring input, the canonical JSON objects, the bundle references and the
receipt. It uses the supplied static bundle and recorded fixtures, not an
invented schema.

## 1. Authoring input: `capsule.toml`

The transformation v0 fixture is a complete small example. `ato pack` reads it
and produces the canonical objects in the next sections.

```toml
schema = "ato.capsule/2"

[application]
title = "Transformation v0 fixture A"
surface_path = "/"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[contract]
mode = "all"
identity = "behavioral"

[[contract.observation]]
id = "root"
use = "ato.contract.http@1"
port = "app.http"
method = "GET"
path = "/"
status = 200
body_digest = "sha256:f345de2490fa7c64f7398b35c26b4591dfa662d3e7815c3a73e591f48559452d"

[[derivation]]
id = "app"
use = "ato.process@1"
argv = ["python3", "app.py"]
cwd = "."
guest_port = 8000
runtimes = { python = "3.12" }

[effects]
default = "pure"
```

- `schema`, `application` and `[[input]]` are authoring input, not identity.
  The input's `content_ref` is resolved to actual bytes when the bundle is
  built.
- `[contract]`: `mode = "all"` means every declared observation is required.
  `identity = "behavioral"` is authoring intent and adds nothing to K's bytes;
  it makes binding refuse a Contract that observes source identity.
  `identity = "source-bound"` requires exactly one workspace identity
  observation, which pins K to a source tree. This `ato.capsule/2` grammar
  refuses `source-bound` and points to `ato.capsule/1`, which can author that
  workspace observation; the supplied static bundle is source-bound in that
  sense.
- Each `[[contract.observation]]` is one requirement: an `id`, a verifier
  (`use`), and verifier-specific fields. This one is an HTTP check on the
  logical Port `app.http` with an expected status and body digest.
- Each `[[derivation]]` is one declared route: an `id`, a route adapter
  (`use = "ato.process@1"`), the command, working directory, Port and runtime
  constraints. Another route would be another `[[derivation]]` block.
- `capsule.toml` is not the Capsule identity. `ato pack` canonicalises the
  declared Contract and each route into the objects below.

## 2. Canonical K: `ato.contract/1`

This is the canonical Contract decoded from
[`samples/interop-static.capsule`](../../samples/interop-static.capsule). Keys are
shown in JCS canonical order, which is the order the digest covers (authoring
uses `capsule.toml`, not this JSON; indentation is for display). Its canonical
bytes are 416 bytes and hash to
`sha256:b565e9d771c53ba03548b1a151c0ebbc2b884c1d6ec532f72596ce33a4b741db`,
which is the bundle's `root_contract_ref`:

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

- `requirements` is a set sorted by `id`; the author's typing order is not part
  of what was chosen to preserve.
- The second requirement is why this K is **source-bound**: it names the
  workspace input and its content digest. The v0 fixture above omits that
  requirement, so its K is behavioral and accepts a different source.
- Contract verifiers in the current tree include `ato.contract.http@1`,
  `ato.contract.workspace@1`, `ato.contract.scenario@1`,
  `ato.contract.tls@1` and `ato.contract.instance-snapshot@1`.
- `ContractRef` is the SHA-256 of the JCS-canonical bytes of this object. The
  referenced bytes are stored in the bundle payloads, so a reader can check the
  digest rather than trusting the index.

## 3. Canonical D: `ato.derivation/1`

The matching static route is also a standalone canonical object (397 bytes,
hashed as `sha256:87a0ac84ccdf72ce5ffde7ffc12277d60f3097fb277332a48173c4ddadb7a3cb`,
the bundle's first `derivations` entry):

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

A process route uses the same shape with `ato.process@1` steps. This is a
simplified excerpt of a recorded canonical derivation (paths shortened and
unrelated fields omitted):

```json
{
  "schema": "ato.derivation/1",
  "effects": "pure",
  "inputs": [
    {"id": "workspace", "protocol": "ato.workspace@1", "content_ref": "sha256:ff99..."}
  ],
  "ports": [
    {"id": "app.http", "protocol": "ato.http@1", "from": "app", "guest_port": 8080}
  ],
  "runtimes": {"node": "22.14.0"},
  "steps": [
    {"id": "dependencies", "protocol": "ato.process@1", "op": "exec", "argv": ["npm", "ci"], "cwd": "."},
    {"id": "app", "protocol": "ato.process@1", "op": "serve", "argv": ["/app/server.mjs"], "cwd": "."}
  ],
  "state": []
}
```

- `inputs[].content_ref` is an immutable object reference. The workspace tree
  and everything it references are part of the bundle closure.
- `ports[].from` names the step that serves the Port. `guest_port` is the
  endpoint inside the realization; the logical Port identity stays `app.http`.
- `steps` keep authored order because a route is a sequence. `requirements`
  carry network/authority constraints that admission checks.
- `DerivationRef` is the SHA-256 of the JCS-canonical bytes of this object.

### Authoring to canonical mapping

| Authoring input | Canonical object |
|---|---|
| `[[contract.observation]].use` | `requirements[].verifier` |
| observation fields (`port`, `method`, `path`, `status`, `body_digest`) | verifier-specific fields on the requirement |
| `identity = "behavioral"` | no bytes; binding-time check only |
| `identity = "source-bound"` | an `ato.contract.workspace@1` requirement (authored with `ato.capsule/1`; the `/2` grammar refuses it) |
| `[[input]]` | `inputs[]` with `content_ref` resolved at build time |
| `[[derivation]].use = "ato.process@1"` | `steps[]` with `protocol = "ato.process@1"` and `op = "exec"` or `"serve"` |
| `argv`, `cwd`, `env` | step fields |
| `guest_port` | `ports[].guest_port` |
| `runtimes` | route-wide `runtimes` map |
| `[effects] default` | `effects` |

## 4. Bundle references

A v3 `.capsule` is a JSON document with an index and base64 payloads. The
supplied static bundle starts like this (abbreviated):

```json
{
  "index": {
    "version": 3,
    "profile": "ato.portable-application/1",
    "root_contract_ref": "sha256:b565e9d7...",
    "application_ref": "sha256:3ed462f6...",
    "derivations": ["sha256:87a0ac84..."],
    "objects": [
      {"kind": "structured", "schema": "ato.contract/1", "reference": "sha256:b565e9d7...", "size": 416},
      {"kind": "structured", "schema": "ato.derivation/1", "reference": "sha256:87a0ac84...", "size": 397}
    ]
  },
  "payloads": [
    {"reference": "sha256:b565e9d7...", "bytes": "<base64 canonical JSON>"}
  ]
}
```

- `root_contract_ref` is the Capsule identity for the portable-application
  profile. It is the digest of the canonical K bytes, not of the file.
- `derivations` lists the declared routes sorted by reference. A bundle can
  declare several; the caller selects one and the receipt names it. Their
  membership, order and success are not part of K.
- The SHA-256 of the whole `.capsule` file is transport equality only. Two
  files can differ while sharing K and D.
- For the computation-root profile (wire v2), the root is `ComputationRef`,
  which is a different domain and must not be read as a `ContractRef`.

## 5. Receipt

A verification receipt is emitted for a Run. This is a trimmed real receipt:

```json
{
  "schema": "ato.contract-verification-receipt/1",
  "target": {"kind": "cli-local"},
  "bundle_sha256": "sha256:5057dbd63868fadc367d0c69da95369d4594d2e57c17f9c79746b1392aa6bd15",
  "contract_ref": "sha256:084a5af45d5761ec2279261b745706459b151bff9786a43d0947f17e4baa7c73",
  "derivation_ref": "sha256:9538c8d1539c6ecd591cae901616906fc294f5fa727767c70a98cf28fa669b74",
  "observations": [
    {
      "id": "backend-health",
      "outcome": "satisfied",
      "evidence": {"method": "GET", "path": "/health", "status": 200, "body_sha256": "sha256:0a749f..."}
    }
  ],
  "fully_satisfied": true,
  "execution": {
    "realization": "oci_service_group",
    "endpoint": "http://127.0.0.1:43181",
    "image": "docker.io/library/python@sha256:c4634f..."
  }
}
```

- Each observation records `satisfied`, `deferred` or `failed`; a deferred
  observation names the gate that decides later.
- `fully_satisfied` is true only when every required observation is
  `satisfied`.
- `execution`, `bundle_sha256` and container IDs are evidence of this Run.
  They are never part of K or D, and a Formation attempt may omit
  `bundle_sha256` because no `.capsule` exists yet.

## 6. What K can express today

K is a set of requirements, and every requirement is required: `mode = "all"`
is the only authoring mode. Each requirement names a verifier. The verifiers in
the current tree are:

| Verifier | Fields | What it checks | What it does not check |
|---|---|---|---|
| `ato.contract.http@1` | `port`, `method` (default GET), `path`, `status`, optional `body_digest` | One HTTP response at a logical Port: the status, and the whole-body SHA-256 when declared. | Headers, JSON structure, other paths or methods, timing, or behavior after the response. |
| `ato.contract.workspace@1` | `input`, `digest` | The exact content reference resolved for that input, i.e. the source closure. | Behavior. A behavioral K omits this requirement. |
| `ato.contract.scenario@1` | The `ato.scenario/1` object pinned by digest; verifier version 1 | An ordered HTTP scenario with status, body and JSON assertions, captures and `fresh_instance` isolation. | Exhaustive API coverage, concurrency, timing, or effects outside the scenario's HTTP steps. |
| `ato.contract.tls@1` | `port`, `server_name`, `start` | The certificate presented on the Port covers the name, is inside its validity window, validates to a public root, and the handshake completes. | Certificate serial or byte pinning; client certificates; policy beyond the chain check. |
| `ato.contract.instance-snapshot@1` | `digest` | The exact restored saved-data snapshot. | Arbitrary files, process memory, or external databases. |

Authoring and binding constraints:

- Requirement ids are unique, and the canonical requirement set is sorted by
  id. Duplicate ids are refused when the document is parsed.
- The authoring grammars accept only the verifier names they know. A verifier
  this build does not know is reported as `verifier_unknown` at verification,
  never silently treated as satisfied.
- `ato.capsule/2` can author HTTP, scenario and TLS observations. A workspace
  identity observation is authored with `ato.capsule/1` (see §1).
- K is sealed at formation. A Run does not recapture or rewrite it. Adding,
  removing or changing a requirement produces different canonical bytes, a
  different `ContractRef`, and therefore a different Capsule identity in this
  profile.

## 7. What identity assurance is guaranteed

These guarantees follow from the format and the verifier, not from a claim that
K describes the whole application:

- **Content addressing.** `ContractRef` is the SHA-256 of the JCS-canonical K
  bytes. Given collision resistance and the same canonicalization, equal refs
  mean an equal requirement set; the bundle file bytes, the chosen D, Instance
  IDs and storage paths do not enter that comparison.
- **Validated closure.** Before realization, the bundle reader checks the
  profile, canonical encoding, descriptor digests and the reachable object
  closure. A payload that does not match its reference, an unknown profile or
  an unreachable required object is rejected rather than partially used.
- **Frozen verification.** A receipt names the `contract_ref` it was verified
  against. The verifier compares a Run with that frozen K; it does not replace
  the expectations with whatever the Run returned.
- **All-or-nothing satisfaction.** `fully_satisfied` is true only when every
  required observation is `satisfied` from actual evidence. `deferred` and
  `failed` do not qualify, and a deferred observation names the gate that must
  decide it later.
- **Source binding when declared.** A K that includes
  `ato.contract.workspace@1` is source-bound: a different source closure fails
  with `input_identity_mismatch`. A behavioral K deliberately allows a different
  source to satisfy the same K.
- **Saved-data binding when declared.** A v4 snapshot requirement binds the
  restored snapshot digest. Different saved data fails with
  `instance_snapshot_mismatch`.
- **Profile separation.** A portable-application `ContractRef` is not a
  computation-root `ComputationRef`. Satisfying K does not turn one profile's
  reference into the other.

## 8. Limits of that assurance

- **K equality is not behavioral equivalence.** There is no bisimulation or
  semantic solver here. Two realizations can satisfy the same K while differing
  in unobserved behavior, performance, resource use, security properties or
  side effects.
- **Two different Ks can be satisfied by the same behavior.** `ContractRef`
  equality tells you the declared conditions match, not that one K is "more
  complete" than another. To compare preservation, compare the canonical
  requirement sets, not the refs alone.
- **K is partial by construction.** Anything not written as a requirement is
  not guaranteed: unobserved endpoints, concurrency, timing, headers, JSON
  shape beyond the scenario assertions, and behavior after the response.
- **K does not bind D.** A bundle may declare several Ds; the receipt names the
  selected one. Another D may satisfy the same K, and D membership, order or
  failure never enters `ContractRef`.
- **Satisfying K is not the same Computation.** A Run that satisfies K is not
  thereby the same Computation or the same Capsule reference as another Run.
  This is the identity boundary described in the README and
  [application contracts](../concepts/application-contracts.md).
- **No continuity guarantee for user data.** Snapshot observation covers the
  declared saved resources only. Restart persistence, volume transfer, browser
  state flush from a live Hosted Instance and a proven Hosted round-trip are
  separate, and some remain unproven (see
  [state continuity](../concepts/state-continuity.md)).
- **Evidence is Run-specific.** HTTP observations are tied to a Port and a
  dynamic endpoint; TLS validity depends on the current clock; execution
  evidence belongs to that Run. A receipt from one host does not certify another
  host or a later Run.
- **Formation can defer.** Scenario, TLS and body-digest checks need a running
  candidate, so a formation-time result may be `deferred`. Only a runtime
  receipt with all observations satisfied is `fully_satisfied`.
- **Canonicalization matters.** The digest covers JCS bytes, not a pretty-printed
  copy. Reformatting, changing key order, or adding whitespace changes the bytes
  you hash even though it does not change the JSON meaning.
- **K is not a security or license attestation.** Contract satisfaction says
  nothing about sandbox strength, permissions, vulnerability status, license
  compliance, or the safety of side effects. Those are separate boundaries.

## 9. What the formats do not contain


- K does not contain the list of Ds, their order, whether they succeeded, an
  Instance or Asset ID, storage paths, grants, signed URLs, secrets, or
  execution evidence. A saved-data snapshot is added as its own Contract
  requirement (`ato.contract.instance-snapshot@1`), producing a new K′.
- D does not contain a reference back to K. The bundle pairs them, and the
  receipt names both.
- `capsule.toml` is authoring input, a `.capsule` file is transport, and a
  snapshot is a materialization. None of them is the Capsule identity.

## See also

- [Capsule bundle overview](../capsule.md) and the
  [wire v3 draft](../rfcs/draft/PORTABLE_APPLICATION_BUNDLE_V3.md).
- [Application contracts](../concepts/application-contracts.md) for the profile
  boundary and [formation and verification](../concepts/formation-verification.md)
  for the verifier and receipt authority.
- Source of truth in the tree: [authoring types](../../lib/formation/src/authoring.rs),
  [`ato.capsule/2` parser](../../lib/formation/src/capsule_toml_v2.rs),
  [verifier and receipt](../../lib/formation/src/verify.rs), and the
  [bundle reader](../../lib/objects/src/portable_bundle.rs).
