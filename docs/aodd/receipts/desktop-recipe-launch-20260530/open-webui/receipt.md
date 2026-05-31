# open-webui (v0.5.10) — Tier B

## AODD Receipt

### Test Date
2026-05-30

### Launch Method
CLI plan-only: `ato run --plan-only samples/recipes/open-webui --yes`

### Result
**BLOCKED** at preflight validation

### Blocker
state_binding_unix_path: `target '/app/backend/data' must be an absolute path`

Open-webui uses `[build.steps]` with pip install and `[[state_bindings]]` for data persistence. The Unix path target fails Windows validation.

Note: An `ato-open-webui` container is already running from a prior CLI session:
```
docker ps → ghcr.io/open-webui/open-webui:main   bash start.sh   Up 8 hours
```
This shows the OCI runtime CAN work when Docker is used directly (not podman).

### Attestations
- [x] CLI preflight blocked by state binding validation
- [x] Docker image is pre-pulled and running (manual `docker run`)
