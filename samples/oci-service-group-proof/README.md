# OCI service group proof

Two services in one `ato.oci@1` Derivation:

- `backend` — `traefik/whoami`, listening on guest port 8081. It declares the
  `data` filesystem state, mounted read-write at `/data`.
- `web` — `nginx-unprivileged`, listening on guest port 8080. It serves `/healthz`
  itself and proxies `/` to `backend:8081`. Only this Port is the Surface
  (`app.http`).

Both images are pinned to `linux/amd64` and the group runs on one `--internal`
Docker network.

```sh
ato pack samples/oci-service-group-proof -o oci.capsule
ato run oci.capsule --no-open --verification-receipt mac.json
```

`ato pack` prints, per declared Derivation, whether **this** host can run it and
why. `ato run` refuses before realizing anything when it cannot, with the same
reason code.

## Platform matrix

The bundle bytes, `ContractRef` and `DerivationRef` are identical on every row.
Only the materialization differs, and the receipt records which one ran
(`execution.connector`, `execution.platform_emulated`).

| Host | Connector | Status | Notes |
|---|---|---|---|
| Linux, native Docker daemon (root) | `direct` | **verified** (CI: Linux service-group tests) | The CLI reaches the container's address on the Run's internal network. |
| macOS, Docker Desktop | `relay` | **verified** (macOS arm64, Docker Desktop 29.1.3) | The daemon is a VM: the container network is not reachable from the CLI, so the Run starts its own relay container on the same internal network and multiplexes the Ports over that container's stdin/stdout. `linux/amd64` images run under emulation and the receipt says `platform_emulated: true`. |
| Linux, rootless Docker | `relay` | not verified on a rootless host | The container network is in a user namespace the CLI cannot enter, so the same relay materialization applies. The decision is made from observed facts (`docker info` rootless security option + endpoint kind), not from a product name. |
| Remote daemon (`tcp://`, `ssh://`) | refused | verified refusal | This host's workspace tree is not on the daemon's machine, so the Run is refused (`remote_daemon_workspace_unsupported`) instead of building a container that would read the wrong tree. |
| Linux, native Docker daemon, no `--internal` reachability (e.g. a custom network setup) | refused | — | A container IP that is not reachable and no relay is not silently papered over. |

## What the Run creates and owns

- One `--internal` Docker network for the Run.
- One container per service, read-only root, `--cap-drop=ALL`,
  `no-new-privileges`, resource limits, no published Ports.
- On hosts that need it, one relay container: built by the CLI from a static
  Linux binary embedded in it, loaded with `docker load` (no registry, works
  offline), attached only to the Run's internal network, read-only root,
  `--cap-drop=ALL`, `no-new-privileges`, non-root, resource-limited, and
  restricted to the `(service, guest_port)` pairs the Derivation declares.

Stopping the Run stops the services, then the relay, then removes the network —
each step confirmed, and the relay container's absence is verified rather than
assumed.

## What the receipt records

```json
{
  "execution": {
    "realization": "oci_service_group",
    "connector": "relay",
    "relay_image_digest": "sha256:...",
    "relay_container_id": "...",
    "platform_emulated": true,
    "services": [
      {"name": "backend", "container_id": "...", "image": "docker.io/traefik/whoami@sha256:..."},
      {"name": "web", "container_id": "...", "image": "docker.io/nginxinc/nginx-unprivileged@sha256:..."}
    ]
  },
  "fully_satisfied": true
}
```

The container ids, the image digests and the connector are evidence of how the
Run happened. None of them enters K or D, and the same bundle produces the same
`ContractRef` and `DerivationRef` on every host.
