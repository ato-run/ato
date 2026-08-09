# Track C PR 2b builder fixtures

Minimal public, no-binding capsules the `snapshot-builder` daemon can materialize + build
end-to-end for validation. `py-web/` is a stdlib-python web app (serves `/health` on 8080).
Pointed at via a stub claim: `{ github_owner: ato-run, github_repo: ato, commit_sha: <sha>,
subdirectory: crates/snapshot-builder/fixtures/py-web }`.

`py-web-bare/` (#932) is the real Store-capsule shape: a bare-`.py` run command and no
explicit readiness_probe — it seals only through bare-.py normalization + probe synthesis
(guest runs `python3 app.py`; the synthesized probe GETs `/`).

`linux-x11-pixel/` is the Dockerfile-import fixture for the authenticated
pixel-stream slice. Its own README documents the explicit private-RFB endpoint
and PID + WM_CLASS + mapped-window + framebuffer readiness gate.

`linux-terminal/` is the deterministic Terminal Surface v1 recipe fixture. One
stdlib Python workload exposes build readiness on `/health` while its controlling
PTY exercises ANSI rendering, keyboard echo, resize/SIGWINCH, Ctrl+C, and clean exit.
