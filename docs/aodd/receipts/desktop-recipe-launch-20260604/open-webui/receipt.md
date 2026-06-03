App: open-webui
Repo: open-webui/open-webui
Platform: windows-x86_64
Desktop build: ato-desktop debug v0.5.5 (dev 0b3f9826, post-#441)
Ato CLI build: target/debug/ato.exe v0.5.5 (post-#441, source_build_shell_unavailable marker present)
Git SHA: 0b3f9826c7852f6c46731c0c6b537395062ed0f6 (dev, post-#441)
ATO_HOME: C:\Users\koh\AppData\Local\Temp\aodd-369-home (clean temp)
Provider: Podman 5.8.2 (podman-machine-default, WSL, rootless)
Launch path used: Desktop omnibar -> NavigateToUrl capsule://github.com/open-webui/open-webui
Recipe source: capsule://github.com/open-webui/open-webui (OCI single container (heavy, ML))
Expected runtime shape: OCI single container (heavy, ML)
Prompts observed: auto
Provider flow: podman ready
Consent flow: auto
Secret flow: none / not reached
Ready signal: NOT reached (no guest-capsule pane)
WebView URL: n/a
Screenshot: n/a
Stop result: no active session to stop
Orphan check: containers clean; per-session podman network left behind (cross-cutting cleanup gap, #450)
Result: SKIPPED_PLATFORM_BLOCKED
First blocker: 4.82GB image present, boot wizard opened, but no container reached Ready / no WebView pane within 200s on the memory-constrained 2GB Podman WSL machine
Follow-up issue: none
Notes: Not a recipe/launch-path defect; resource-constrained host. Re-test on a machine with more RAM allocated to the Podman VM.
