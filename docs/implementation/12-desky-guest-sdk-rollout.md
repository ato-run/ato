# 12. Desky Guest SDK Rollout

## Goal

Reduce framework-specific boilerplate in Desky guest apps by introducing thin workspace-local SDK layers for backend runtime setup and frontend bridge helpers, while keeping the existing guest contract and Desky shell behavior intact.

## Scope Completed

### Backend SDK packages

- `packages/desky-guest-tauri`
- `packages/desky-guest-wails`
- `packages/desky-guest-electron-backend`

These packages now centralize:

- `ATO_GUEST_MODE` and `DESKY_SESSION_*` environment parsing
- dual-mode startup helpers
- `/health` and `/rpc` HTTP guest server bootstrapping
- built-in `ping` and `check_env` handling
- boundary policy helpers for workspace-relative file access
- graceful shutdown wiring

### Frontend helper package source

- `packages/desky-guest-frontend`

This package is the source-of-truth for framework-specific browser bridge helpers for Tauri, Wails, and Electron.

## What Was Verified

### Tauri

- backend SDK crate compiled
- real Tauri sample compiled after SDK migration
- ignored E2E passed: `cargo test --test desky_session_e2e desky_session_roundtrip_for_real_tauri_sample -- --ignored --nocapture`
- Desky smoke passed: `npm --prefix apps/desky run electron:sample:tauri`

### Wails

- Wails SDK package passed `go test ./...`
- real Wails sample backend passed `go test ./...`
- ignored E2E passed after cache warm: `cargo test --test desky_session_e2e desky_session_roundtrip_for_real_wails_sample -- --ignored --nocapture`
- Desky smoke passed: `npm --prefix apps/desky run electron:sample:wails`

### Electron

- Electron backend SDK passed `node --check`
- ignored E2E passed: `cargo test --test desky_session_e2e desky_session_roundtrip_for_real_electron_sample -- --ignored --nocapture`
- Desky smoke passed: `npm --prefix apps/desky run electron:sample:electron`

### Shared frontend helper behavior

- Tauri, Wails, and Electron guest frontends still reached `frontend-mode` and `frontend-echo`
- `lastGuestMode == "1"` remained true in Desky smoke for all three frameworks
- boundary-policy rejection for `../README.md` remained fail-closed
- CSP probe remained blocked

## Important Constraint

Desky currently serves guest browser assets only from the directory rooted at `frontend_entry` via `capsule://<session_id>/...`.

Because of that, browser helper modules outside the guest frontend root are intentionally not importable at runtime. The practical shape is therefore:

- package source in `packages/desky-guest-frontend`
- runtime helper copies vendored under each sample frontend
- sample-local `bridge.js` files as thin re-export shims

## Known Operational Note

The first cold Wails guest-mode run may hit readiness timeout while `go run` downloads dependencies. Once the Go module cache is warm, the same ignored E2E passes and the runtime path is healthy.

This is currently a cold-start cost issue, not a guest contract mismatch.

## Out Of Scope For This Commit

- workspace-root `samples/` changes
- workspace-root `docs/specs/` updates
- automated sync tooling for frontend vendored helper copies

Those changes exist in the workspace but are not part of the `apps/ato-cli` git repository.