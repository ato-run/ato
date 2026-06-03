# #377 regression receipt — Windows source-build /bin/sh shell dependency

Platform: windows-x86_64 (RDP session)
Git SHA: 0b3f9826c7852f6c46731c0c6b537395062ed0f6 (dev, post-#441 merge)
Ato CLI: target/debug/ato.exe v0.5.5 (post-#441; `source_build_shell_unavailable` marker present in binary)
Provider: Podman 5.8.2 (podman-machine-default, WSL)
ATO_HOME: C:\Users\koh\AppData\Local\Temp\aodd-369-home (clean temp)

## Claim under test
After #441, on Windows:
1. Known catalog recipes launched by GitHub URL resolve to the OCI/runtime
   recipe path and never fall to the raw source-build `/bin/sh` path.
2. An unregistered repo whose source-build path needs a Unix shell returns a
   typed `E213 source_build_shell_unavailable`, not a generic E999 / raw
   `os error 2`.

## Evidence 1 — known recipes take the OCI path (no /bin/sh)
Driven through the Desktop omnibar (NavigateToUrl), GitHub-URL form:

| Handle | Result | WebView |
|---|---|---|
| capsule://github.com/excalidraw/excalidraw | guest-capsule @ http://127.0.0.1:37983/ | Excalidraw canvas UI |
| capsule://github.com/usememos/memos | guest-capsule @ http://127.0.0.1:39425/ | Memos signup UI |
| capsule://github.com/sosedoff/pgweb | guest-capsule @ http://127.0.0.1:38453/ | pgweb connection form |

All three resolved to catalog OCI recipes, started real containers, and
rendered app UI. No `/bin/sh` / `sh` spawn failure anywhere. (Screenshots in
the sibling app receipt dirs.)

## Evidence 2 — unregistered repos take the source-build path (typed errors)
- `capsule://github.com/octocat/Hello-World` (not in catalog) →
  `ATO_ERR_MANUAL_INTERVENTION_REQUIRED` (inference) after generating a preview
  capsule.toml — a typed error, not E999, not a /bin/sh crash. See
  `octocat_hello_world*.json`.
- linkwarden / langflow (not in catalog) → `resolved GitHub source reference`
  (raw source-build), confirming the resolver split.

## Evidence 3 — source-build prestart needing /bin/sh → E213 (live run)
A minimal local source capsule with a shell prestart (`echo step-a && echo
step-b`) was run on Windows with **no POSIX shell on PATH** (PowerShell, not
git-bash):

```
ato run <probe> --yes --dangerously-skip-permissions --json   (CAPSULE_ALLOW_UNSAFE=1)
```

Result (see `prestart_shell_probe.json`):
```json
{"code":"E213","name":"source_build_shell_unavailable","phase":"provisioning",
 "message":"source_build_shell_unavailable: this source-build / prestart / smoke step
   requires a POSIX shell (/bin/sh), which is not available on platform=windows.
   requested: `echo step-a && echo step-b`. ...",
 "cleanup_status":"complete"}
```

Typed, actionable, cleanup completed — NOT E999, NOT a bare `os error 2`. This
exercises the #441 prestart guard end-to-end on a real Windows run.

## Result
result: complete — #377 regression is resolved on Windows. Known catalog
recipes never hit the /bin/sh source-build path; unregistered source-build that
needs a Unix shell returns the typed E213.

Note: this is a regression receipt, not a matrix app row (per #369 scope).
