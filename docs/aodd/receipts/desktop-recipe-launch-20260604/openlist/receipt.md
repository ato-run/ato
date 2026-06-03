App: openlist
Repo: openlistteam/openlist
Platform: windows-x86_64
Desktop build: ato-desktop debug v0.5.5 (dev 0b3f9826, post-#441)
Ato CLI build: target/debug/ato.exe v0.5.5 (post-#441, source_build_shell_unavailable marker present)
Git SHA: 0b3f9826c7852f6c46731c0c6b537395062ed0f6 (dev, post-#441)
ATO_HOME: C:\Users\koh\AppData\Local\Temp\aodd-369-home (clean temp)
Provider: Podman 5.8.2 (podman-machine-default, WSL, rootless)
Launch path used: Desktop omnibar -> NavigateToUrl capsule://github.com/openlistteam/openlist
Recipe source: capsule://github.com/openlistteam/openlist (OCI (openlist-google-drive-crypt))
Expected runtime shape: OCI (openlist-google-drive-crypt)
Prompts observed: secret required
Provider flow: podman ready
Consent flow: secret required
Secret flow: required (Google Drive crypt) - not provided
Ready signal: NOT reached (no guest-capsule pane)
WebView URL: n/a
Screenshot: n/a
Stop result: no active session to stop
Orphan check: containers clean; per-session podman network left behind (cross-cutting cleanup gap, #450)
Result: SKIPPED_UNSUITABLE
First blocker: recipe requires 1 secret (Google Drive crypt config); automated run did not provide it ('1 required secret(s) - run: ato app config set github.com/openlistteam/openlist')
Follow-up issue: none
Notes: Resolves to the openlist-google-drive-crypt catalog recipe; needs external Google Drive credentials, out of scope for an unattended AODD run.
