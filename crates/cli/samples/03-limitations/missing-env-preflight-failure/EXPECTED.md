# Expected Behavior — Pending ato-cli Fix

## Status: ⚠️ Pending ato-cli fix

**File this issue on ato-run/ato-cli when gh auth is available:**

> **Title**: `required_env is advisory-only; auto-provisioner synthesizes dummy values`
>
> **Labels**: `spec-alignment`, `samples-blocked`
>
> **Body**: In v0.4.69, `required_env = ["OPENAI_API_KEY"]` does not block execution when
> `OPENAI_API_KEY` is absent. The auto-provisioner synthesizes a 15-char placeholder and
> proceeds to execution (exit 0). Expected behavior: pre-launch error with dedicated code
> (e.g. E220) before auto-provisioner or consent gate runs.

## Current observed behavior (v0.4.69)

```
Auto-provisioning issue [app]: prepare synthetic env for OPENAI_API_KEY [safe-default]
```

Script executes with a fake key → **exit 0** (unexpected)

## Expected behavior after fix

```
E220: Required environment variable OPENAI_API_KEY is not set.
Add it to your environment before running this capsule.
```

→ **exit 1** (non-zero)

## What changes in this sample once the fix ships

- `health.toml`: `exit_nonzero = false` → `exit_nonzero = true`
- `health.toml`: add `stderr_contains = "OPENAI_API_KEY"`
- This `EXPECTED.md` will be removed
