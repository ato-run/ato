# `required_env` is advisory-only; auto-provisioner synthesizes dummy values

<!--
LABELS: spec-alignment, samples-blocked
REPO: ato-run/ato-cli
-->

## Summary

In v0.4.69, declaring `required_env = ["OPENAI_API_KEY"]` in `capsule.toml` does **not** prevent execution when `OPENAI_API_KEY` is absent. The auto-provisioner synthesises a 15-character placeholder and execution continues, silently running the capsule with a fake credential.

This breaks the "Safe by default" philosophy and makes the manifest declaration ineffective as a safety gate.

## Observed behavior (v0.4.69)

Given a capsule with:
```toml
required_env = ["OPENAI_API_KEY"]
```

Running without `OPENAI_API_KEY` set:
```
Auto-provisioning issue [app]: prepare synthetic env for OPENAI_API_KEY [safe-default]
```
→ **exit 0** — script runs with a synthetic placeholder.

## Expected behavior

Before the consent gate and auto-provisioner run, ato should emit a dedicated error for each absent required env var:
```
E220: Required environment variable OPENAI_API_KEY is not set.
      Declare it in your shell or add it to a .env file before running this capsule.
      See https://ato.run/docs/errors#e220
```
→ **exit 1**

## Proposed fix

1. Add a new `E220` error code for "required env var absent".
2. In `execute_plan` (or the pre-consent check), iterate `required_env` before reaching the auto-provisioner.  
   If any declared var is absent **and** its name is not already in the auto-provisioner allowlist, emit E220 and abort.
3. Optional: add `[env].enforcement = "strict" | "advisory"` to `capsule.toml` (default `strict`) so power users can opt-in to the old advisory behaviour if needed.

## Reproducer

```bash
cd samples/03-limitations/missing-env-preflight-failure
unset OPENAI_API_KEY
ato run .  # should exit 1 with E220, currently exits 0
```

Sample: `samples/03-limitations/missing-env-preflight-failure`  
Tracking: `samples/03-limitations/missing-env-preflight-failure/EXPECTED.md`

## Impact

- `03-limitations/missing-env-preflight-failure` sample currently produces a false-pass (documents gap via `EXPECTED.md`).
- Any capsule relying on `required_env` as a safety guard is silently bypassed.
- Severity: **High** — spec/implementation divergence; user-facing security posture claim is not enforced.
