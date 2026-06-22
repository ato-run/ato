# Test Fixtures for Security and Runtime Validation

This directory contains test-only capsule fixtures used by CI and local test runs.

## Purpose

- Reproduce fail-closed behavior for risky or malformed inputs.
- Validate policy enforcement and runtime isolation.
- Prevent regressions in security-sensitive code paths.

Examples include scenarios such as:

- simulated malicious package behavior
- network exfiltration attempts
- web path traversal checks
- sandbox and lockfile validation

## Safety Notes

- These fixtures are not production capsules.
- They are intentionally crafted for negative/security testing.
- Do not publish or deploy these fixtures to production environments.
