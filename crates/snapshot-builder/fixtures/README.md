# Track C PR 2b builder fixtures

Minimal public, no-binding capsules the `snapshot-builder` daemon can materialize + build
end-to-end for validation. `py-web/` is a stdlib-python web app (serves `/health` on 8080).
Pointed at via a stub claim: `{ github_owner: ato-run, github_repo: ato, commit_sha: <sha>,
subdirectory: crates/snapshot-builder/fixtures/py-web }`.
