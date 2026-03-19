# ADR 0001: Error Taxonomy and Structured Diagnostics

## Status

Accepted

## Context

The CLI currently collapses too many distinct failures into coarse buckets, especially E102 and E999. That makes the product harder to operate in three places:

- interactive recovery cannot choose the right repair flow
- CI and agents cannot distinguish retryable failures from user-actionable ones
- regression analysis cannot separate inference issues from provisioning and execution failures

The repository already contains partially structured failure signals:

- CLI diagnostics in src/diagnostics.rs
- execution-plan error codes in core/src/execution_plan/error.rs
- free-form manual intervention messages in src/inference_feedback.rs

Those signals are inconsistent. Some errors are typed, others are inferred from string matching, and the JSON error envelope does not carry enough metadata for agentic recovery.

## Decision

We standardize error handling around two principles.

1. E codes represent recovery flow, not every root cause detail.
2. Detailed cause data is emitted as machine-readable metadata in details.

The top-level taxonomy is phase-oriented.

- 0xx: manifest authoring
- 1xx: inference and plan generation
- 2xx: provisioning and supply chain
- 3xx: execution, environment, and policy
- 9xx: internal failures

The canonical CLI-facing code set is:

- E001 ManifestTomlParse
- E002 ManifestSchemaInvalid
- E003 ManifestRequiredFieldMissing
- E101 EntrypointInvalid
- E102 ManualInterventionRequired
- E103 MissingRequiredEnv
- E104 DependencyLockMissing
- E105 AmbiguousEntrypoint
- E106 StrictManifestFallbackBlocked
- E107 UnsupportedProjectArchitecture
- E201 AuthRequired
- E202 PublishVersionConflict
- E203 DependencyInstallFailed
- E204 RuntimeCompatibilityMismatch
- E205 EngineMissing
- E206 SkillNotFound
- E207 LockfileTampered
- E208 ArtifactIntegrityFailure
- E209 TlsBootstrapRequired
- E210 TlsBootstrapFailed
- E211 StorageNoSpace
- E301 SecurityPolicyViolation
- E302 ExecutionContractInvalid
- E303 RuntimeNotResolved
- E304 SandboxUnavailable
- E305 RuntimeLaunchFailed
- E999 InternalError

E102 and E999 remain as compatibility fallbacks during migration, but no new implementation should target them when a more specific code exists.

## Canonical Model

The core library owns the typed error catalog.

- capsule-core defines a rich AtoError enum
- execution-plan and executor layers flatten AtoError into AtoExecutionError for transport
- CLI diagnostics map AtoExecutionError and CapsuleError into user-facing E codes

Each structured error exposes:

- code
- name
- phase
- message
- hint
- retryable
- interactive_resolution
- resource
- target
- details

## JSON Contract

The machine-readable envelope is:

```json
{
  "schema_version": "1",
  "status": "error",
  "error": {
    "code": "E103",
    "name": "missing_required_env",
    "phase": "inference",
    "message": "Required environment variables are missing.",
    "hint": "Try running with -e KEY=VALUE or use the interactive prompt.",
    "retryable": false,
    "interactive_resolution": true,
    "details": {
      "missing_keys": ["DATABASE_URL", "STRIPE_KEY"],
      "target": "server"
    }
  }
}
```

This envelope is designed for both TTY and non-TTY modes.

- TTY mode uses code and details to route into repair prompts
- JSON mode emits the full envelope unchanged for CI and agents

## Policy Violation Split

The previous ATO_ERR_POLICY_VIOLATION bucket is too broad. It is split as follows.

- ATO_ERR_SECURITY_POLICY_VIOLATION maps to E301
- ATO_ERR_EXECUTION_CONTRACT_INVALID maps to E302
- ATO_ERR_RUNTIME_NOT_RESOLVED maps to E303

The legacy ATO_ERR_POLICY_VIOLATION code remains as a temporary fallback for call sites not yet migrated.

Examples:

- blocked egress in Deno or Node compat becomes E301
- readiness probe shape errors and IPC contract failures become E302
- unresolved deno, node, python, or uv runtime selection becomes E303

## Migration Plan

1. Introduce the typed AtoError catalog in capsule-core.
2. Enrich AtoExecutionError with phase, name, retryability, and details.
3. Update CLI diagnostics to map typed errors directly instead of relying on string matching.
4. Convert high-volume sources first:
   - policy_violation call sites
   - manual intervention inference paths
   - integrity and TLS bootstrap failures
5. Keep E102 and E999 only as explicit fallback paths.

## Consequences

Positive:

- structured recovery becomes possible without brittle string parsing
- CI can branch on actionable categories
- test corpora can be bucketed by recovery path instead of generic failure labels

Trade-offs:

- the error catalog is larger and must be maintained deliberately
- diagnostics mapping becomes more explicit
- some legacy tests asserting old generic codes must be updated during migration

## Implementation Notes

- Avoid adding per-package-manager or per-runtime E codes unless they change recovery flow.
- Prefer details.package_manager, details.runtime, details.lockfile, and details.blocked_host over new codes.
- Keep manifest parsing and schema validation separate from provisioning and execution failures.
