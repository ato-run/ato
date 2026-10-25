# Formation 6b-D1 — native process vs Dockerfile→OCI qualification

Status: **qualification only** (docs). No implementation, build, image pull,
run, network change or model call; no deploy/remote migration. Base `233bda52`.

Question: for the 41 service/process sources of the 50-app wave, which route
would increase generic coverage more — native process authoring, or authoring
a bounded source-Dockerfile build into Ato's existing OCI execution? Inputs:
[corrected D0](formation-service-qualification-d0.json) and a static parse of
every Dockerfile in the same pinned archives
([facts](formation-oci-source-qualification-d1-facts.json),
`scripts/acceptance/coverage/dockerfile-qualification-facts.py`). Ledger
generator: `scripts/acceptance/coverage/oci-source-qualification-d1.py`.

The Dockerfile parser is a static survey extractor (256 KiB reads, simplified
instruction parsing); it is not a safety validator and never authorizes a
build. D0 input: #1444 merged `2ce9a0eb` (runtime in catalog 17/41 =
declared 13 + Ato default 4).

Current code facts: the OCI Adapter runs a **verified, digest-pinned image
archive** offline; no path forms an image from a source Dockerfile. A
Dockerfile is an untrusted build program; this document only classifies.

## OCI v0 structural checks (strict)

- docker-default root Dockerfile selected (no variant choice)
- final stage = default target
- FROM ARGs only with in-file defaults; no platform-injected ARGs
- no remote ADD, no privileged/host-network/device RUN, no secret/ssh mounts
- final stage CMD or ENTRYPOINT (left to OCI semantics, never converted to native argv)
- exactly one literal TCP EXPOSE
- no mandatory external service
- no Binding needed for startup (e.g. config absent from the image)

CMD/ENTRYPOINT stay Docker/OCI semantics and are never translated into native
argv.

## Distribution (one primary bucket per app)

Precedence fixed before counting: external service > OCI v0 eligible > native blocked only by port binding > runtime catalog > ambiguous.

| Bucket | Apps |
|---|---|
| native_process_possible_now | 0 |
| dockerfile_oci_possible_if_build_added | 7 |
| blocked_runtime_catalog | 12 |
| blocked_port_binding | 4 |
| blocked_external_service | 13 |
| blocked_dependency_network | 0 |
| ambiguous | 5 |

- **native_process_possible_now = 0**: no source has a native port contract, and
  the network stays denied.
- **blocked_dependency_network = 0 as a primary bucket** only because it is
  universal: all 41 need dependency resolution (native install, or image build
  = base pull + RUN installs). It gates every route and is recorded per app.

Route-level views: native blocked_runtime_catalog 16, blocked_external_service 13, blocked_port_binding 6, blocked_launch_undeclared 6;
OCI blocked_external_service 13, structural_candidate 9, ambiguous_selection 9, no_dockerfile 5, ambiguous_port 3, no_port 2.

## Structural OCI candidates (static only)

These are **static candidates, not executable-verified apps**: nothing has been
built, started, answered HTTP or passed a safety review.

- structural_candidate: **9** (all structural checks pass)
- without a startup Binding or external dependency: **7**
  (the count reported by the first revision; the primary bucket above)
- with no unresolved requirement at all: **6**

`EXPOSE` is recorded as `declared_transport_port`: port metadata, not proof of
an HTTP service, a response or permission to publish. HTTP suitability is
decided by the Runtime and the Verifier. Unresolved requirements are typed
(`startup_binding`, `external_dependency`, `privilege`) and must be resolved
before execution; none is treated as passed.

| App | Runtime | Declared transport port | Stages | Bases digest-pinned | Unresolved requirements |
|---|---|---|---|---|---|
| WBO | node | 80 | 1 | no (tag) | – |
| changedetection.io | python | 5000 | 2 | no (tag) | – |
| Etherpad | node — outside catalog | 9001 | 6 | no (tag) | – |
| Vikunja | go — outside catalog | 3456 | 3 | yes | – |
| Grist core | node — outside catalog | 8484 | 6 | no (tag) | privilege: runtime sandbox (gVisor via sandbox/run.sh) may require container privileges beyond the OCI Adapter default; unverified |
| File Browser | go — outside catalog | 80 | 2 | no (tag) | – |
| Kutt | node | 3000 | 1 | no (tag) | – |
| PrivateGPT | python | 8080 | 6 | no (tag) | external_dependency: settings.yaml defaults select a RabbitMQ task broker (PGPT_TASKS_RESULTS_BROKER_MODE:rabbitmq) and an external LLM; startup without them is unverified |
| Glance | go — outside catalog | 8080/tcp | 2 | no (tag) | startup_binding: ENTRYPOINT reads /app/config/glance.yml, which the final stage does not copy (only the binary) |

Four of the primary-bucket seven (Etherpad Node 24, Vikunja Go, Grist Node 22.12,
File Browser Go) are **blocked on the native route by the runtime catalog**; the
image would bring its own runtime. Grist stays in the bucket only as a
structural observation: its container-privilege question is unresolved.

## Comparison

- Native route after a new port-binding primitive: at most **6** (WBO, Kutt,
  JupyterLab, Radicale, Calibre-Web, PrivateGPT) — all Node/Python; the other 35
  stay blocked by catalog, undeclared launch or external services.
- OCI route after a bounded Dockerfile build: **7** structural candidates in the primary bucket (6 without any unresolved requirement), spanning Node,
  Python and Go, without widening the language runtime catalog. Nine more have
  Dockerfiles but no docker-default root file (ambiguous selection), three have
  multiple/variable ports and two declare none; Glance needs a startup Binding
  and PrivateGPT an unverified external dependency.
- Union of both: 11. External services (13) block both routes equally.

Costs and risks recorded, not solved: an OCI build route needs a contained
builder for an untrusted build program, registry egress for base images and RUN
installs, and Ato-owned freezing of tag→digest (**19 of 20 root Dockerfiles pull bases by tag; an OCI build route must resolve and freeze digests as Ato-owned facts**).
The native route needs a declared launch+port binding that sources do not
provide today, plus the same dependency acquisition policy.

## Not decided here

The 6b-D2 target is a separate decision. Nothing was implemented; network stays
denied; no image was built or pulled.
