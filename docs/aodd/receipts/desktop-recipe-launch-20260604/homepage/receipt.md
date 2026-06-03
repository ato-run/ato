App: homepage
Repo: gethomepage/homepage
Platform: windows-x86_64
Desktop build: ato-desktop debug v0.5.5 (dev 0b3f9826, post-#441)
Ato CLI build: target/debug/ato.exe v0.5.5 (post-#441, source_build_shell_unavailable marker present)
Git SHA: 0b3f9826c7852f6c46731c0c6b537395062ed0f6 (dev, post-#441)
ATO_HOME: C:\Users\koh\AppData\Local\Temp\aodd-369-home (clean temp)
Provider: Podman 5.8.2 (podman-machine-default, WSL, rootless)
Launch path used: Desktop omnibar -> NavigateToUrl capsule://github.com/gethomepage/homepage
Recipe source: capsule://github.com/gethomepage/homepage (OCI single container (Node))
Expected runtime shape: OCI single container (Node)
Prompts observed: auto
Provider flow: podman ready
Consent flow: auto
Secret flow: none / not reached
Ready signal: guest-capsule pane bound to http://127.0.0.1:38839/ after 45s
WebView URL: http://127.0.0.1:38839/
Screenshot: screenshot.png
Stop result: stopped=true
Orphan check: containers clean; per-session podman network left behind (cross-cutting cleanup gap, #450)
Result: DEGRADED
First blocker: homepage rejects the 127.0.0.1:<port> Host header; recipe does not set HOMEPAGE_ALLOWED_HOSTS
Follow-up issue: #446 (HOMEPAGE_ALLOWED_HOSTS)
Notes: WebView renders homepage's own error screen rather than the dashboard.
