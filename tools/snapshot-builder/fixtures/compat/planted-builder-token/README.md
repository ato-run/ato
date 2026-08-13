# planted-builder-token — how this row is enforced

The guarantee: the builder's own live credentials (exact values, >=16 bytes)
appearing in ANY produced artifact GATE the seal at `no_secret_scan`.

A pinned public git source **cannot** carry the live staging token (committing
it would be the leak the gate exists to catch), so this row is enforced in two
places instead of via source content:

1. **Unit level (in-repo, always on):** the #935 guard tests in
   `tools/snapshot-builder/src/main.rs` plant the live credential value into a
   scanned artifact tree and assert the gate refuses the seal.
2. **API E2E (staging):** the E2E harness injects the fault at build time —
   it writes a file containing the staging builder token into the builder's
   work dir for this fixture's job (post-materialize hook) and asserts the job
   acks `failed` with `failure_stage = no_secret_scan`.

The capsule here is otherwise sealable — proving the failure comes from the
planted token, not from the app.
