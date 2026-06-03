App: blinko
Repo: blinkospace/blinko
Platform: windows-x86_64
Desktop build: ato-desktop debug v0.5.5 (dev 0b3f9826, post-#441)
Ato CLI build: target/debug/ato.exe v0.5.5 (post-#441, source_build_shell_unavailable marker present)
Git SHA: 0b3f9826c7852f6c46731c0c6b537395062ed0f6 (dev, post-#441)
ATO_HOME: C:\Users\koh\AppData\Local\Temp\aodd-369-home (clean temp)
Provider: Podman 5.8.2 (podman-machine-default, WSL, rootless)
Launch path used: Desktop omnibar -> NavigateToUrl capsule://github.com/blinkospace/blinko
Recipe source: capsule://github.com/blinkospace/blinko (OCI 2-service (app + postgres:14))
Expected runtime shape: OCI 2-service (app + postgres:14)
Prompts observed: auto
Provider flow: podman ready
Consent flow: auto
Secret flow: none / not reached
Ready signal: NOT reached (no guest-capsule pane)
WebView URL: n/a
Screenshot: n/a
Stop result: no active session to stop
Orphan check: containers clean; per-session podman network left behind (cross-cutting cleanup gap, #450)
Result: FAIL
First blocker: postgres 'db' service exits: chmod: changing permissions of '/var/lib/postgresql/data': Operation not permitted; initdb cannot fix permissions on the bind-mounted data dir (rootless Podman/WSL). Surfaced as E999 'orchestration services failed to start in-process' (cause: service 'db' exited before readiness check passed).
Follow-up issue: #444 (state bind-mount ownership) + #445 (E999 vs typed exited-before-ready)
Notes: Same mount-permission root cause as node-red. No containers after stop (clean).
