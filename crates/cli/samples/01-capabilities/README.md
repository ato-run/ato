# Tier 01 — Capabilities

One `ato` feature per sample. Each sample is the smallest program that meaningfully exercises the capability, and its README explains exactly *what* the capability is and *why* you'd use it.

## Samples (planned)

| Slug | Runtime | Capability exercised |
|---|---|---|
| `workspace-share-roundtrip` | `web/static` | `ato workspace share` + `ato workspace setup` |
| `watch-mode-live-reload` | `source/node` | `--watch` |
| `background-ps-logs` | `source/node` | `--background` + `ato ps` / `logs` / `stop` |
| `ipc-python-node` | `source/python` + `source/node` | `ato ipc` cross-language |
| `network-policy-allowlist` | `source/node` | `allow_domains` (positive case) |
| `env-preflight` | `source/node` | Required-env fail-closed |
| `share-url-from-github` | `web/static` | `ato run github.com/<owner>/<repo>@<sha>` |
