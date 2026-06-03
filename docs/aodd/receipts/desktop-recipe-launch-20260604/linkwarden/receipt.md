App: linkwarden
Repo: linkwarden/linkwarden
Platform: windows-x86_64
Desktop build: ato-desktop debug v0.5.5 (dev 0b3f9826, post-#441)
Ato CLI build: target/debug/ato.exe v0.5.5 (post-#441, source_build_shell_unavailable marker present)
Git SHA: 0b3f9826c7852f6c46731c0c6b537395062ed0f6 (dev, post-#441)
ATO_HOME: C:\Users\koh\AppData\Local\Temp\aodd-369-home (clean temp)
Provider: Podman 5.8.2 (podman-machine-default, WSL, rootless)
Launch path used: Desktop omnibar -> NavigateToUrl capsule://github.com/linkwarden/linkwarden
Recipe source: capsule://github.com/linkwarden/linkwarden (app+postgres (intended))
Expected runtime shape: app+postgres (intended)
Prompts observed: n/a
Provider flow: n/a
Consent flow: n/a
Secret flow: none / not reached
Ready signal: NOT reached (no guest-capsule pane)
WebView URL: n/a
Screenshot: n/a
Stop result: no active session to stop
Orphan check: containers clean; per-session podman network left behind (cross-cutting cleanup gap, #450)
Result: SKIPPED_MISSING_RECIPE
First blocker: not registered in the bundled sample-recipe catalog (SAMPLE_RECIPE_CATALOG); GitHub handle resolved to raw source-build, which has no capsule.toml -> preflight failed
Follow-up issue: #449 (register linkwarden/langflow in catalog)
Notes: Confirms the #377 split from the other side: unregistered handles take the raw GitHub source-build path.
