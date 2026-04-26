# Pending Upstream Issues

Issues to be filed against `ato-run/ato-cli`. Kept here until `gh auth` is configured, at which point `tools/sync-upstream-issues.sh` will file them automatically.

## Filing Instructions

**Manual**: open each file, copy the body, and create the issue at <https://github.com/ato-run/ato-cli/issues/new>.

**Automated** (once `gh auth login` is done):
```bash
bash tools/sync-upstream-issues.sh
```

## Index

| # | File | Title | Status | Labels |
|---|------|-------|--------|--------|
| 001 | [001-required-env-advisory.md](001-required-env-advisory.md) | `required_env` is advisory-only; auto-provisioner synthesizes dummy values | pending manual filing | `spec-alignment`, `samples-blocked` |
| 002 | [002-llama-local-chat-tracking.md](002-llama-local-chat-tracking.md) | \[tracking\] llama-local-chat sample pending GPU broker v1 API | pending manual filing | `tracking`, `gpu-broker` |
