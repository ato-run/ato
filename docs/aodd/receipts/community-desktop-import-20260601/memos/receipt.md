# Memos Receipt

## Command

`ato app session start github.com/usememos/memos --community-toml-id ctoml_01ksza4mjs1feg3gwvx9cy35w5 --attach-state data:<worktree>/.tmp/ato-aodd-state-rerun/memos/data --json`

## Result

- Status: pass
- Session: `ato-desktop-session-48971`
- Local URL: `http://127.0.0.1:38407/`
- Runtime: OCI orchestration, target `app`
- Manifest: `.tmp/ato-aodd-home-rerun/memos/cache/community-tomls/ctoml_01ksza4mjs1feg3gwvx9cy35w5.toml`
- Stop: clean

## Notes

The explicit persistent `data` binding was accepted and applied after community TOML provenance validation. Startup reached ready and stopped cleanly.
