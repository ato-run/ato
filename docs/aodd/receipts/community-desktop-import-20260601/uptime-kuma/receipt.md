# Uptime Kuma Receipt

## Command

`ato app session start github.com/louislam/uptime-kuma --community-toml-id ctoml_01ksza4mvjtk8wthttjk3h2zxv --attach-state data:<worktree>/.tmp/ato-aodd-state-rerun/uptime-kuma/data --json`

## Result

- Status: pass
- Session: `ato-desktop-session-36940`
- Local URL: `http://127.0.0.1:35633/`
- Runtime: OCI orchestration, target `app`
- Manifest: `.tmp/ato-aodd-home-rerun/uptime-kuma/cache/community-tomls/ctoml_01ksza4mvjtk8wthttjk3h2zxv.toml`
- Stop: clean

## Notes

The explicit persistent `data` binding was accepted and applied after community TOML provenance validation. Startup reached ready after database patching and stopped cleanly.
