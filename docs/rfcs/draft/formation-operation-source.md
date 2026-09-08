# Formation operation source sidecar

Status: Draft; companion to ato-api#589 and ato-pwa#349. No deployment.

The API claim response may add `operation_catalog_required: true` outside the
canonical FormationJob. Missing means false, preserving compatibility with old
control planes. The API requests this only for Static Web output when operation
registration is configured. The job's verified, selected Source Closure is the
source of operation code; built output is insufficient because static artifact
pruning intentionally removes authoring YAML.

Before detection/build, the Worker scans the verified root for root
`ato.operations.yaml`, JavaScript modules and HTML discovery candidates. It sends
only that bounded UTF-8 sidecar to the attempt-authenticated
`/v1/internal/formation/attempts/:id/operation-source` route. It never decompresses
large assets again at the API and never evaluates operations itself. The API's
single registrar owns strict parsing, canonical IR, closure bundling and isolated
execution validation. App-specific operation semantics stay outside Kernel.

The scan is limited to 10,000 filesystem entries, 65 selected files and 576 KiB
(including a maximum 64 KiB declaration). Symlinks and invalid UTF-8 are refused;
hidden/credential directories and node_modules do not contribute. Source Closure
verification/capture policy remains the prior boundary. A root declaration makes
an incomplete scan fatal. Without one, an unsupported/over-budget scan returns no
operations and a diagnostic, allowing normal app Formation to continue. This
conservative v0 can reject large explicit source trees with unused JS; targeted
module negotiation is future work.

The endpoint uses existing owner/Runner authentication and current attempt
fencing. Registration failure stops Formation; missing required catalog prevents
Schema sealing even with an older Worker. There is no source-discovery fallback
from a malformed explicit declaration. Sidecars are per attempt; the accepted
job's catalog becomes immutable on its Schema. API changes and migrations must
be reviewed with this Worker before a separately authorized rollout.

Validation: collector tests cover a 6 MiB untransferred image and explicit versus
implicit scan-limit failure. Existing formation-worker tests, cargo fmt and
clippy remain required. API tests cover current/stale attempt publication and
missing/invalid catalog refusal. Local API acceptance uses a Formation result
fixture; it does not claim a deployed Runner end-to-end test.
