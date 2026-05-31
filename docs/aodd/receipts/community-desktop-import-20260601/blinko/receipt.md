# Blinko Receipt

## Command

`ato app session start github.com/blinkospace/blinko --community-toml-id ctoml_01ksza4ndct49z3qdsvs73p4g1 --attach-state db-data:<worktree>/.tmp/ato-aodd-state-rerun/blinko/db-data --attach-state app-data:<worktree>/.tmp/ato-aodd-state-rerun/blinko/app-data --json`

## Result

- Status: blocked
- Blocker: community TOML schema hash format
- Error: `Configuration error: Schema hash has invalid format: sha256:blinko-pgdata-v1`

## Notes

The previous missing explicit state binding blocker was cleared after community TOML provenance validation. The remaining blocker is narrower and is caused by the community TOML declaring a non-digest `schema_id`.
