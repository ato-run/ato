App: excalidraw
Repo: excalidraw/excalidraw
Platform: windows-x86_64
Desktop build: ato-desktop debug v0.5.5 (dev 0b3f9826, post-#441)
Ato CLI build: target/debug/ato.exe v0.5.5 (post-#441, source_build_shell_unavailable marker present)
Git SHA: 0b3f9826c7852f6c46731c0c6b537395062ed0f6 (dev, post-#441)
ATO_HOME: C:\Users\koh\AppData\Local\Temp\aodd-369-home (clean temp)
Provider: Podman 5.8.2 (podman-machine-default, WSL, rootless)
Launch path used: Desktop omnibar -> NavigateToUrl capsule://github.com/excalidraw/excalidraw
Recipe source: capsule://github.com/excalidraw/excalidraw (OCI single container (nginx static))
Expected runtime shape: OCI single container (nginx static)
Prompts observed: auto
Provider flow: podman ready
Consent flow: auto
Secret flow: none / not reached
Ready signal: guest-capsule pane bound to http://127.0.0.1:37983/ after 10s
WebView URL: http://127.0.0.1:37983/
Screenshot: screenshot.png
Stop result: stopped=true
Orphan check: containers clean; per-session podman network left behind (cross-cutting cleanup gap, #450)
Result: PASS
First blocker: none
Follow-up issue: none
Notes: #377 regression anchor: GitHub URL -> catalog OCI recipe, NO /bin/sh source-build. Full Excalidraw UI rendered. Clean after stop.
