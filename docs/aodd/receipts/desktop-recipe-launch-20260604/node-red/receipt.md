App: node-red
Repo: node-red/node-red
Platform: windows-x86_64
Desktop build: ato-desktop debug v0.5.5 (dev 0b3f9826, post-#441)
Ato CLI build: target/debug/ato.exe v0.5.5 (post-#441, source_build_shell_unavailable marker present)
Git SHA: 0b3f9826c7852f6c46731c0c6b537395062ed0f6 (dev, post-#441)
ATO_HOME: C:\Users\koh\AppData\Local\Temp\aodd-369-home (clean temp)
Provider: Podman 5.8.2 (podman-machine-default, WSL, rootless)
Launch path used: Desktop omnibar -> NavigateToUrl capsule://github.com/node-red/node-red
Recipe source: capsule://github.com/node-red/node-red (OCI single container (Node))
Expected runtime shape: OCI single container (Node)
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
First blocker: container exits at startup: EPERM: operation not permitted, copyfile '/usr/src/node-red/node_modules/node-red/settings.js' -> '/data/settings.js' (state bind-mount not writable by container user on rootless Podman/WSL)
Follow-up issue: #444 (Windows/Podman state bind-mount ownership)
Notes: No container at ready or after stop (clean). Same mount-permission class as blinko.
