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

Current code facts: the OCI Adapter runs a **verified, digest-pinned image
archive** offline; no path forms an image from a source Dockerfile. A
Dockerfile is an untrusted build program; this document only classifies.

## OCI v0 subset (strict)

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
OCI blocked_external_service 13, ambiguous_selection 9, v0_eligible 7, no_dockerfile 5, ambiguous_port 3, no_port 2, requires_binding 2.

## OCI v0-eligible (7)

| App | Runtime | EXPOSE | Stages | Bases digest-pinned | Flags |
|---|---|---|---|---|---|
| WBO | node | 80 | 1 | no (tag) | – |
| changedetection.io | python | 5000 | 2 | no (tag) | FROM uses ARG PYTHON_VERSION with default 3.11 |
| Etherpad | node — outside catalog | 9001 | 6 | no (tag) | final stage chosen by ARG BUILD_ENV=git default (build_${BUILD_ENV}) |
| Vikunja | go — outside catalog | 3456 | 3 | yes | – |
| Grist core | node — outside catalog | 8484 | 6 | no (tag) | runtime sandbox (gVisor via sandbox/run.sh) may assume extra container privileges; unverified |
| File Browser | go — outside catalog | 80 | 2 | no (tag) | – |
| Kutt | node | 3000 | 1 | no (tag) | shell-form CMD runs `npm run migrate && npm start` inside the container |

Four of the seven (Etherpad Node 24, Vikunja Go, Grist Node 22.12, File Browser
Go) are **blocked on the native route by the runtime catalog**; the image brings
its own runtime. WBO and Kutt are eligible on both routes; changedetection.io
has no native launch declaration but a Dockerfile CMD.

## Comparison

- Native route after a new port-binding primitive: at most **6** (WBO, Kutt,
  JupyterLab, Radicale, Calibre-Web, PrivateGPT) — all Node/Python; the other 35
  stay blocked by catalog, undeclared launch or external services.
- OCI route after a bounded Dockerfile build: **7** v0-eligible, spanning Node,
  Python and Go, without widening the language runtime catalog. Nine more have
  Dockerfiles but no docker-default root file (ambiguous selection), three have
  zero/multiple/variable ports, two need a startup Binding.
- Union of both: 11. External services (13) block both routes equally.

Costs and risks recorded, not solved: an OCI build route needs a contained
builder for an untrusted build program, registry egress for base images and RUN
installs, and Ato-owned freezing of tag→digest (**19 of 20 root Dockerfiles pull bases by tag; an OCI build route must resolve and freeze digests as Ato-owned facts**).
The native route needs a declared launch+port binding that sources do not
provide today, plus the same dependency acquisition policy.

## Not decided here

The 6b-D2 target is a separate decision. Nothing was implemented; network stays
denied; no image was built or pulled.
