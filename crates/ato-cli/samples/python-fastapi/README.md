# Python FastAPI Sample

Minimal FastAPI capsule sample for source/python execution.

## Prerequisites

- `uv.lock` must exist before `ato build .`
- Tier2 run requires sandbox opt-in
- If auto-bootstrap cannot resolve the compatible nacelle release, pass an installed engine explicitly

## Commands

```bash
ato init . --yes
ato build .
ato run . --sandbox --nacelle ~/.cargo/bin/nacelle
```

## Notes

- `ato build .` prepares hermetic Python cache artifacts from `uv.lock`
- `ato run . --sandbox` may still try nacelle auto-bootstrap depending on local engine compatibility policy; `--nacelle ~/.cargo/bin/nacelle` is the reliable override
