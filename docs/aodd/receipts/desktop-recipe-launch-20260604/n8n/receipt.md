App: n8n
Repo: n8n-io/n8n
Platform: windows-x86_64
Desktop build: ato-desktop debug v0.5.5 (dev 0b3f9826, post-#441)
Ato CLI build: target/debug/ato.exe v0.5.5 (post-#441, source_build_shell_unavailable marker present)
Git SHA: 0b3f9826c7852f6c46731c0c6b537395062ed0f6 (dev, post-#441)
ATO_HOME: C:\Users\koh\AppData\Local\Temp\aodd-369-home (clean temp)
Provider: Podman 5.8.2 (podman-machine-default, WSL, rootless)
Launch path used: Desktop omnibar -> NavigateToUrl capsule://github.com/n8n-io/n8n
Recipe source: capsule://github.com/n8n-io/n8n (OCI single container (Node+SQLite))
Expected runtime shape: OCI single container (Node+SQLite)
Prompts observed: auto
Provider flow: podman ready
Consent flow: auto
Secret flow: none / not reached
Ready signal: guest-capsule pane bound to http://127.0.0.1:33245/ after 145s
WebView URL: http://127.0.0.1:33245/
Screenshot: screenshot.png
Stop result: stopped=true
Orphan check: containers clean; per-session podman network left behind (cross-cutting cleanup gap, #450)
Result: DEGRADED
First blocker: readiness probe passes on n8n's HTTP 'starting up' splash before the editor/setup UI is ready; screenshot captured the splash, not demo-ready UI
Follow-up issue: #448 (n8n readiness vs startup splash)
Notes: Container up, WebView serves n8n's own content (not blank/error). Slow first boot (~145s).
