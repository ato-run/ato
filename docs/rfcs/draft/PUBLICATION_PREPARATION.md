# Publication preparation and evidence

Status: Draft

A publication preparation is local orchestration. The owner selects a
workspace and a target. Ato snapshots source before formation or copies an
existing portable bundle. It records the source closure, the exact artifact's
SHA-256, its original Contract and Derivations, and the intended destination.
The artifact is verified and uploaded as the same bytes. Changing a condition,
destination, or artifact starts a new preparation.

A client can propose required HTTP status and body digest checks. For plain
static HTML or a Python application without a manifest, Ato compiles them
into the initial portable Contract. The client may propose up to four Python
routes using a source-relative script, a pinned interpreter version, a guest
port and optional argv entries. The script can read
`ATO_ENDPOINT_APP_HTTP_PORT` or receive the selected port as an argv entry
that matches the guest port. Formation validates and seals the source
without executing the program or installing dependencies. Adding an
alternative route changes the artifact and its declared Derivations while
retaining K. A bundle or authored source retains its declared Contract and
routes. A proposed HTTP check on those targets must already be declared
exactly or is reported as unsupported.
Use an `ato.capsule/2` manifest with an `ato.process@1` route and
`[[derivation.service]]` entries to form a native process service group.
Set an empty `runtimes = {}` on the route. Each service declares its own
Python version, Ports and `connect` edges.
The preparation shows the serving Ports and peer edges by service and keeps
the authored K and D unchanged.
An authored `ato.capsule/2` can define a single OCI container or an OCI
service group. Supply `oci_archives` as image-reference and path pairs
within the authorized workspace. Keep the archives beside the source tree.
Each archive must match the pinned manifest, platform, and complete config
and layer graph. The compiler embeds the verified archives before freezing
the final artifact. Repacking preserves the original K and D. It creates
a distinct transport digest, so the repacked artifact gets its own
verification and approval. Preparation needs no registry access.
An optional Browser Contract v0 is fixed before execution, with the effective
Contract referring to the original portable Contract and the browser condition.
Unrecognized required checks prevent approval.

A macOS desktop preparation can freeze a `native_window` task with a
`criterion` and one to four exact button labels in `press`. Its
`ato.capsule/2` route declares `ato.native@1`,
`ato.native.presentation = "local-window"`, a bundled `.app` tarball and
`signature_target = "bundle"`. The original K and D remain fixed.
`/ready` observes that the Run owns an on-screen window. The desktop
criterion uses separate evidence from the window.

The verification worker on macOS 14 or later needs an active desktop session,
Accessibility authorization and Screen Recording authorization. Seatbelt
keeps the executable and source read-only, scopes writable state to the
attempt, and blocks network and child process creation. The window stays on
the host's WindowServer. The verifier limits capture to the selected process's
single on-screen window and sends `AXPress` only to a unique enabled button
in its accessibility tree. ScreenCaptureKit and Vision collect before/after
OCR evidence for the pinned local Ollama judge. A durable report includes
the judge decision, citations and OCR digests. The raw pictures and
transcribed text are discarded.

A valid Developer ID bundle uses its declared signature policy. When a
valid ad-hoc signature is allowed by the route, `ato publish authorize-native
<preparation-id>` records the owner's terminal consent for the fixed
artifact before verification. Upload still needs report approval and
confirmed stop. The recipient must make its own decision about running an
ad-hoc signed artifact.

The local worker verifies declared Derivations within one fixed budget, using
the normal portable executor and attempt journal. An isolated verification Run
uses fresh state, with no credentials, saved Instances, or external dependency
retrieval authorized by the publication workflow. The requested host triple
defaults to the local worker. Each attempted route reports its actual host,
and a worker on a different host cannot verify the preparation. The report
lists the declared routes, platforms, runtime pins, dependency profiles, and
required recipient binding names.

The v0 browser helper requires measured Linux containment. A live observation
names the session, current observation and eligible elements. A click or
input must reference the same current document and observed element. After
each operation, the client takes a new observation. Navigation stays within
the application origin and input accepts only synthetic values in supported
controls. The client's finish operation sends observations to the independent
judge. A preparation can select the contained model-based driver instead.
The worker pins driver and judge identities before running. The verifier
distinguishes browser snapshots, model extractions and action reports.
An out-of-scope request, mismatched evidence, absent receipt, or unavailable
judge prevents a pass. Any change to the task or model selection requires a
new preparation.

The durable owner-only record retains typed condition verdicts, attempt and
environment identity, evidence identifiers and digests, model identity, and
cleanup outcomes. Live page excerpts and synthetic browser inputs are available
only during the session. The helper uses private temporary scratch for its
mailbox, cleared on normal teardown. Its configured model provider sees the
bounded evidence needed for evaluation and uses its own data retention policy.
The durable report stores evidence digests instead of raw browser evidence.
An owner-held key authenticates local records. On Unix, new publication
directories use owner-only permissions. Other systems rely on the account's
permissions for the local Ato home. Records and frozen artifacts remain there
until removed by the owner. Abrupt interruption can leave scratch files.
This local integrity protection trusts processes that can read the key. A
missing worker after execution begins leaves an inconclusive preparation.
Cancellation is cooperative, with cleanup tracked independently.

Approval is a terminal operation. The owner reads the report, confirms
redistribution rights and reviews the exact artifact for private data, then
approves its report digest. An initial natural-language request authorizes
preparation. Upload requires the matching approval, exact artifact, and
unchanged destination. A positively identified embedded Instance snapshot
or sensitive filename blocks this workflow. Other data absence remains
unconfirmed by automated inspection. Link possession grants decryption and
the link is retained as private result data. Public Hub versions are pinned
by digest. A possibly completed upload is marked unknown. Reconciliation can
check the current public Hub tag against the exact artifact and can recover
a matching result. An encrypted upload with a lost response has no link to
recover through this command.

## Local clients

Run the MCP server with the absolute path to the authorized workspace. The
client and the terminal used for approval must share the same Ato home.

```sh
codex mcp add ato-publication -- /path/to/ato publish mcp --workspace /path/to/source
claude mcp add --transport stdio ato-publication -- /path/to/ato publish mcp --workspace /path/to/source
```

Ask the agent to prepare a publication after reading the authorized source.
It proposes checks, uses `publication_prepare`, starts verification, and
resumes status by preparation ID. A browser task uses
`publication_browser_observe` and `publication_browser_act` while verification
is underway. The owner reviews the report and the exact artifact named by
the approval command. The agent can request the upload once an approval
appears in the report. The publication result identifies a route with
confirmed verification and includes the explicit `ato run --derivation`
selection for recipients.

The manual route accepts the same request JSON shown by the MCP tool schema.

```sh
ato publish prepare request.json --workspace /path/to/source
ato publish start "$preparation_id"
ato publish wait "$preparation_id"
ato publish approve "$preparation_id" --report-ref "$report_ref"
ato publish commit "$preparation_id" --report-ref "$report_ref"
```

For browser tasks, configure the absolute helper directory using
`ATO_PUBLICATION_BROWSER_VERIFIER` and install that helper's locked
dependencies. The Linux worker needs usable bubblewrap, Chrome, and system
fonts so Chrome can observe visible text.
`ATO_BROWSER_CHROME_PATH` and `ATO_PUBLICATION_NODE` can select local binaries.
Set `JEV_API_KEY` for the independent judge, and `DEEPSEEK_API_KEY` when using
the model-based browser driver. These keys are forwarded to the contained
helper by file descriptor. Optional `ATO_JEV_MODEL` and
`ATO_BROWSER_AGENT_MODEL` settings select the model identities recorded in
the preparation and receipt.

For structured browser verification the owner can configure a local Ollama
service using `ATO_OLLAMA_JUDGE_URL`, `ATO_OLLAMA_JUDGE_MODEL` and
`ATO_OLLAMA_JUDGE_DIGEST`. The URL is a literal HTTP loopback origin with
an explicit port. The model is a preinstalled tagged local model and the
digest is the 64-character lowercase value from `GET /api/tags`. The
preparation pins all three. The helper checks the digest before and after
each bounded, schema-validated decision. Browser and application traffic
are restricted independently from the judge connection. The report names
the digest as returned by the owner-operated service. This measures the
service's advertised identity and depends on that service's integrity.

## Current scope

This lane accepts self-contained portable bundles and portable authoring
manifests and can form Static Web and pinned single-process Python routes
from source. Candidate exploration uses the available declared routes
within the fixed time and attempt budget. Static Web, Linux Python
processes and process service groups, single OCI containers, OCI service
groups, and bundled macOS local-window apps have bounded verification
launch paths.

The Linux process route requires bubblewrap PID and mount namespaces plus
Landlock network policy. The worker resolves and measures an installed
interpreter before launch. The isolated process sees a read-only source
and private Run storage. Its namespace stops with the owned supervisor.

For native process service groups, publication verification additionally
isolates each service in a separate network namespace with only loopback.
The Runtime forwards observation traffic through an owner-held ingress
listener. It connects consuming services through fixed bridges for the
declared peer Ports. A readiness check waits for the workload's Port
instead of the host listener. Runtime discovery, ordered startup and
readiness share the preparation deadline. After expiry the worker still
stops and reaps every started service before removing scratch. Unconfirmed
cleanup leaves the attempt inconclusive. The recipient uses the same D
through the normal portable Run entry point.

OCI verification requires a self-contained offline archive for every image
and a directly reachable native Linux Docker Engine. The attempt carries
one command deadline across image loading, network creation, group launch,
and readiness. A private Docker client targets the measured local socket.
The containers share an internal network and the browser uses the declared
HTTP surface. Cleanup has a separate budget. The worker leaves a
preparation inconclusive if removal of owned containers or networks cannot
be confirmed.

The native window lane accepts bundled `.tar.gz` applications.
The frozen upload is limited to 8 MiB.
The review receipt remains in the local preparation store. Hosting review
receipts and admission enforcement in the Hub require server support.
