# stale-lockfile

This sample ships an `ato.lock.json` whose `lock_id` hash (`blake3:0000...`) does not match the hash ato would compute from the current `capsule.toml`. Running `ato run .` immediately fails with E999 "lock_id mismatch" before any provisioning or execution occurs. It demonstrates ato's fail-closed lockfile integrity check: a stale or tampered lock file is detected and rejected, never silently ignored.
