# Fixed-purpose SDK environment projection

Status: draft. Implemented portable validation/projection only, not deployed.

Existing `ApplicationBindingV1.protocol` explicitly opts into two projections:
`ato.openai.embeddings.api-key@1` → `OPENAI_API_KEY`, and
`ato.openai.embeddings.base-url@1` → `OPENAI_BASE_URL`. The api-key scalar is a
short-lived per-Run fixed-operation capability, never a provider credential.
An application declares each Binding; selected Process/OCI services explicitly
consume it through existing Derivation bindings. Other protocols, including
unknown versions, retain their existing `ATO_BINDING_*` environment names.

Validation refuses duplicate projected environment targets and collisions with
declared Process or service environment variables. The validator's required
Binding report and runtime resolution use the same projection. Secret values
remain runtime inputs and never enter bundle bytes, K or D. Existing bundles
and the legacy JSON descriptor marker are unchanged; there is no implicit
injection or arbitrary JSON interpretation.

106 portable-library tests and all-target Clippy pass. Tests cover explicit
projection, retained opaque descriptors, duplicate/collision rejection, unknown
protocol versions, and unchanged K/D after runtime resolution. API projection
and SDK representation compatibility are separate required receiver changes.
Network authorization and actual private workload relay remain a subsequent
slice; recognizing the protocol does not grant network access. No provider,
operator key, production or staging actions were performed.
