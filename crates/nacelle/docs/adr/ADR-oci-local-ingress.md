# ADR: Local Ingress for OCI Service Endpoints

| Field        | Value                                   |
| ------------ | --------------------------------------- |
| Status       | Complete (PR 2 — local path router merged) |
| Date         | 2026-05-24                              |
| Scope        | OCI runtime, session lifecycle, Desktop |
| Supersedes   | —                                       |
| Superseded by| —                                       |

---

## 1. Context

OCI capsules in Ato run as containers on per-session bridge networks. Each
container exposes service ports that Ato maps to dynamically allocated host
ports via `EndpointAllocator` (`127.0.0.1:0` binding). This allocation is
safe, collision-resistant, and explicitly excluded from execution identity.

However, host ports are **unstable across sessions** and **opaque to
application code**. Several classes of multi-service apps break under this
model:

- **Frontend apps** that need to know the backend URL at runtime
  (`CONSOLE_API_URL`, `API_BASE_URL`).
- **Desktop UX** where "open app" must resolve to a predictable endpoint.
- **Multi-service OCI graphs** where services communicate through an upstream
  reverse proxy (e.g., Dify uses nginx to route `/` -> web, `/api` -> api).

Ato already ships an axum-based reverse proxy (`binding/proxy.rs`) used for
manual ingress bindings (`ato binding serve-ingress`), but there is no
automatic ingress layer that starts with the session and presents a unified
endpoint surface.

### Platform considerations

On macOS and Windows, OCI runtimes (Podman, Docker Desktop) run containers
inside a virtual machine. Host port mapping traverses an additional NAT layer,
making direct container access from the host browser dependent on correctly
published ports. Any local ingress must operate on the host side of this
boundary.

### Routing approaches

Two broad approaches exist for multi-service local routing:

- **Path-based**: `http://127.0.0.1:<port>/<session>/<service>/` — no DNS, no
  hosts file, uniform across platforms.
- **Hostname-based**: `http://<session>.ato.localhost/` — clean origin
  separation but requires DNS or hosts-file cooperation.

Both have distinct trade-offs documented in section 4.

---

## 2. Goals

- Provide a **session-stable local endpoint** for each Ato session (see
  Stability Scoping below).
- Support **multi-service apps** with a frontend/backend split (e.g., Dify,
  Supabase, Appsmith).
- Avoid requiring **fixed host ports** — allocation remains dynamic.
- Avoid **host network** mode and **privileged containers**.
- Preserve **Ato-managed lifecycle**: ingress starts and stops with the
  session.
- Keep endpoint information **out of execution identity** unless explicitly
  declared as launch envelope input (per the three-domain identity model).
- Work with both **CLI** (`ato run`, `ato ps`) and **Desktop** (open-app UX).
- Reuse the existing axum reverse proxy infrastructure where possible.

### Stability scoping

"Stable" in this ADR means **stable within a single running session**, not
across session restarts. Specifically:

```text
Stable within one session:
  - After container host-port allocation completes, the named route surface
    does not change for the rest of the session.
  - Container service host ports may be allocated dynamically, but the session
    display endpoint is always the router URL.
  - Env-injected URLs (e.g., CONSOLE_API_URL) remain valid for the entire
    session lifetime.

NOT stable across sessions:
  - Router port changes between sessions (dynamic allocation).
  - Session ID changes between sessions (ephemeral).
  - URLs are not bookmarkable across session restarts.
```

The phrase "survives port re-allocation" in earlier drafts was misleading.
The accurate statement is: **container service host ports may change between
sessions, but within a session, all endpoints converge on the router URL as
the single entry surface.**

---

## 3. Non-goals

- Public internet exposure.
- TLS certificate management in v1 (existing self-signed bootstrap is
  orthogonal).
- Kubernetes Ingress compatibility or IngressClass API surface.
- Full-featured reverse proxy (rate limiting, auth middleware, caching).
- Cross-capsule service mesh (capsule-to-capsule communication is mediated
  by the IPC broker, not by ingress).
- Arbitrary host-level routing changes requiring admin privileges (e.g.,
  modifying `/etc/hosts` or installing system DNS resolvers).
- **Build-time frontend env resolution**: variables like `NEXT_PUBLIC_*` that
  are baked into static assets at image build time are out of scope for v1.
  Env injection in this ADR targets **runtime-phase** environment variables
  only — values that take effect at container start or via entrypoint template
  expansion. See section 5 for the phase classification.
- **Full interactive Dify pass as guaranteed outcome**: this ADR proposes an
  architecture that makes Dify interactive pass *plausible*, but path-based
  routing with apps that assume `/` ownership is a known risk. See section 9.

---

## 4. Design Options

### A. Path-based local router

A single HTTP reverse proxy on a dynamically allocated host port. Requests
are routed by path prefix to upstream container services.

**URL model:**

```
http://127.0.0.1:<router-port>/i/<session>/<service>/
```

**Pros:**

- No DNS setup, no hosts file edits, no admin privileges.
- Works identically on macOS, Linux, and Windows.
- Simple to launch: one port, one process.
- Reuse existing `proxy.rs` axum infrastructure with route-table additions.
- Session-scoped isolation by path prefix.

**Cons:**

- Base-path rewriting: apps that generate absolute paths or use `<base href>`
  may break without configuration.
- Apps that assume `/` ownership (e.g., `/_next/...`, `/assets/...`,
  `/signin`) will generate absolute-path requests that bypass the session
  prefix. The browser sends `/_next/...` instead of
  `/i/<session>/web/_next/...`.
- Cookies default to the full origin, not the path scope — cross-service
  cookie leakage is possible within the same origin.
- CORS is trivial (same origin) but cookie-based session sharing across
  path-mounted services requires careful `Path` attribute handling.

### B. Hostname-based local router

Route by hostname (`Host` header) instead of path prefix.

**URL model:**

```
http://<session>.ato.localhost/          -> web service
http://api.<session>.ato.localhost/      -> api service
```

**Pros:**

- Clean origin separation: each service gets its own origin, eliminating
  cookie/CORS/base-path issues.
- Matches how production reverse proxies typically work.
- Better fit for apps that assume they own the entire origin.

**Cons:**

- Requires DNS resolution: either `/etc/hosts` entries (admin), a local DNS
  resolver (e.g., `dnsmasq`), or reliance on `*.localhost` wildcard behavior
  (platform-dependent, not universally supported).
- macOS `*.localhost` resolution is not guaranteed in all browsers.
- Windows `*.localhost` may resolve to `::1` instead of `127.0.0.1` depending
  on browser.
- TLS termination for per-host certificates adds future complexity.
- Platform differences make v1 support burdensome.

### C. Stable allocated host ports per service

Reserve and persist port mappings in the session/lock layer so that the same
service always gets the same host port.

**Pros:**

- Simplest model: no proxy, no routing.
- Direct container access.

**Cons:**

- Collision-prone: the OS may allocate a "reserved" port between sessions.
- Weak UX: users see bare port numbers, not named services.
- Does not solve the "frontend needs to know backend URL" problem unless the
  port is known before the frontend container starts (circular dependency).
- Persisted port mappings add host-specific state to what should be a portable
  session layer.
- Host port values should not enter execution identity lightly; stable
  allocation tempts that boundary.

### D. Injected env endpoint map (session-computed)

Before container start, Ato computes the full endpoint URL map from allocated
ports and injects it as environment variables.

**Env model:**

```
CONSOLE_API_URL=http://127.0.0.1:<api-host-port>/
FILES_URL=http://127.0.0.1:<files-host-port>/
```

**Pros:**

- Directly solves the Dify `CONSOLE_API_URL` class of problems.
- No proxy required for simple cases: containers receive their dependency URLs
  before startup.
- Compatible with the existing `{{deps.<name>.runtime_exports}}` template
  mechanism.
- Allocated ports remain session-layer (not identity-layer).

**Cons:**

- Requires knowing env var names per app (recipe-specific configuration).
- Port allocation must complete before any dependent container starts —
  constrains startup ordering.
- URLs use raw host ports that change between sessions; any bookmarked or
  cached URL becomes stale.
- Does not provide a single stable entry point for Desktop "open app" UX.
- If env var names/templates are declared in the manifest, they become
  identity-bearing — must be carefully scoped.

---

## 5. Recommended Decision

**v1: Path-based local router + optional runtime env injection.**

Implement a session-scoped local ingress proxy that:

1. Starts automatically as part of `ato run` for OCI sessions that declare an
   `[ingress]` block.
2. Routes requests by path prefix to upstream container services.
3. Computes a session-stable endpoint map that downstream services can consume
   via environment variables.
4. Presents a single "open app" URL for Desktop and CLI use.

**v2 consideration:** Hostname-based routing for better origin separation,
deferred until platform DNS handling is validated.

### Rust type model

The manifest-level types are defined first; TOML is their projection.

```rust
use std::collections::BTreeMap;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IngressMode {
    Path,
    Host,
}

impl IngressMode {
    pub fn validate_v1(&self) -> Result<(), IngressError> {
        match self {
            IngressMode::Path => Ok(()),
            IngressMode::Host => Err(IngressError::UnsupportedInV1 {
                mode: "host".into(),
                message: "hostname-based ingress is deferred to v2".into(),
            }),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IngressConfig {
    pub mode: IngressMode,
    pub routes: BTreeMap<String, IngressRoute>,
    #[serde(default)]
    pub env_inject: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IngressRoute {
    pub target: String,
    pub port: u16,
    #[serde(default)]
    pub listed: bool,
    #[serde(default)]
    pub alias: Option<String>,
    #[serde(default = "default_strip_prefix")]
    pub strip_prefix: bool,
    #[serde(default)]
    pub upstream_path_prefix: Option<String>,
    #[serde(default)]
    pub root: bool,
}

fn default_strip_prefix() -> bool {
    true
}
```

`IngressMode::Host` is parsed and persisted but rejected at validation time
in v1. This preserves forward compatibility without using `#[serde(skip)]`,
which would silently discard the user's input.

`env_inject` is a two-level map: outer key is the target service name (the
container that receives the env), inner key is the env variable name, and the
value is a template string resolved at session startup.

### Route name vs alias

The key under `[ingress.routes.<name>]` is the **stable route identifier**
used by `env_inject` templates (e.g., `{{ingress.routes.api.url}}` references
the route named `api`). The `alias` field controls the **URL path segment**
(e.g., `/i/<session>/api/`). If `alias` is omitted for a non-root route, the
route name is used as the alias. Route names and aliases may differ, but
templates always reference the route name, not the alias.

### TOML projection

```toml
[ingress]
mode = "path"                    # "path" (v1) | "host" (v2, parsed but rejected)

[ingress.routes.web]
target = "web"                   # service name from [services]
port = 3000                      # container port
listed = true                    # shown in session index and Desktop UI
root = true                      # this route owns /i/<session>/* fallback
strip_prefix = true              # strip /i/<session>/ before forwarding

[ingress.routes.api]
target = "api"
port = 5001
listed = false                   # reachable via path, hidden from index
alias = "api"                    # path segment: /i/<session>/api/
strip_prefix = true              # strip /i/<session>/api/ before forwarding
upstream_path_prefix = "/api"    # prepend /api after stripping

[ingress.env_inject]
# Into the "web" container, set CONSOLE_API_URL to the api route's ingress URL
web.CONSOLE_API_URL = "{{ingress.routes.api.url}}"
web.API_BASE_URL = "{{ingress.routes.api.url}}"
```

### Root route

A `root = true` route owns the session prefix directly:
`/i/<session>/*` (no alias segment). This is for apps that assume `/`
ownership and generate absolute paths like `/_next/...`, `/assets/...`,
`/signin`.

```toml
[ingress.routes.web]
target = "web"
port = 3000
listed = true
root = true
```

**Important limitation:** A root route does not fix apps that emit
absolute-path URLs such as `/_next/...`. When a browser encounters an
absolute URL beginning with `/`, it requests `/_next/...` from the origin
root, not `/i/<session>/_next/...`. Therefore, root routes only help apps
that use relative paths, respect a configurable base path, or are otherwise
configured to generate session-prefixed URLs.

This is the fundamental limitation of path-based routing for `/`-owning apps.
Three mitigation strategies exist:

1. **App supports runtime base path**: the app is configured to prepend its
   base path to all generated URLs. This works for apps with configurable
   `BASE_PATH` / `basePath` settings.
2. **Route is mounted at session root with `root = true`**: the route catches
   all paths under `/i/<session>/*`. This works **only if the app generates
   relative paths or respects `<base href>`**. Apps that hardcode absolute
   paths to `/` will still break.
3. **Router rewrites response content**: the router intercepts HTML/JS and
   rewrites absolute paths. This is fragile and out of scope for v1.

v1 supports strategies 1 and 2. Strategy 3 is explicitly a non-goal. Apps
that do not support configurable base paths and generate absolute-path URLs
require hostname-based routing (v2) for full compatibility.

### Path rewrite contract

The path-based router must define exactly what happens to the request path
between the client and the upstream service. This is the most critical
behavioral contract for v1.

**Routing anatomy:**

```
Client request:  GET /i/<session>/api/chat/messages
                        ├───┐     ├──┤  ├──────────────┤
                        session  alias  remaining path
```

**Default behavior (strip_prefix = true, no upstream_path_prefix):**

1. Match the longest route alias prefix: `/i/<session>/<alias>/`.
2. Strip the matched prefix.
3. Forward `/<remaining>` to the upstream.

```
Request:  /i/abc123/api/chat/messages
Route:    alias="api", strip_prefix=true
Forward:  /chat/messages -> http://127.0.0.1:<api-host-port>/chat/messages
```

**With upstream_path_prefix:**

1. Match and strip the route alias prefix (same as above).
2. Prepend `upstream_path_prefix` to the remaining path.
3. Forward to the upstream.

```
Request:  /i/abc123/api/chat/messages
Route:    alias="api", strip_prefix=true, upstream_path_prefix="/api"
Forward:  /api/chat/messages -> http://127.0.0.1:<api-host-port>/api/chat/messages
```

**With strip_prefix = false:**

1. Match the route alias prefix for routing only.
2. Forward the full path unchanged.

```
Request:  /i/abc123/api/chat/messages
Route:    alias="api", strip_prefix=false
Forward:  /i/abc123/api/chat/messages -> http://127.0.0.1:<api-host-port>/i/abc123/api/chat/messages
```

**Root route behavior (root = true):**

1. Match any path under `/i/<session>/` that is not matched by a more
   specific alias route.
2. Strip `/i/<session>/` and forward the remaining path.

```
Request:  /i/abc123/_next/static/chunk.js
Route:    root=true (web), strip_prefix=true
Forward:  /_next/static/chunk.js -> http://127.0.0.1:<web-host-port>/_next/static/chunk.js
```

Root routes act as the fallback within the session prefix. Alias routes take
priority over root routes by longest-prefix-match.

**Trailing slash handling:**

- Alias routes are matched with a trailing slash: `/i/<session>/<alias>/`.
- A request to `/i/<session>/<alias>` (no trailing slash) receives a `308
  Permanent Redirect` to the slash-terminated path.
- This avoids ambiguity and ensures consistent path matching.
- Root routes match `/i/<session>/` and `/i/<session>/*` without redirection.
- A request to `/i/<session>` (no trailing slash) receives a `308 Permanent
  Redirect` to `/i/<session>/`. This ensures stable relative-URL resolution
  for all routes under the session prefix.

### Route URL template semantics

Templates in `env_inject` resolve to concrete URLs at session startup. The
resolution rules must be unambiguous to avoid path-joining errors (e.g.,
accidental `/api/api/chat` duplication).

**Available template variables per route:**

```text
{{ingress.routes.<name>.url}}           # http://127.0.0.1:<router>/i/<session>/<alias>/
                                        #   Always slash-terminated.
                                        #   For root routes: http://127.0.0.1:<router>/i/<session>/

{{ingress.routes.<name>.origin}}        # http://127.0.0.1:<router>

{{ingress.routes.<name>.path}}          # /i/<session>/<alias>/
                                        #   For root routes: /i/<session>/

{{ingress.routes.<name>.base_url}}      # http://127.0.0.1:<router>/i/<session>/<alias>
                                        #   No trailing slash. Useful for apps that append
                                        #   their own path segments.
```

**Path-joining rule:** when a frontend app constructs a URL from an injected
variable, the app is responsible for correct joining. `{{url}}` is
slash-terminated; apps that append `/chat` get
`.../api/chat` (correct). Apps that append `/api/chat` get
`.../api/api/chat` (incorrect — recipe must adjust).

The recipe author must verify how the target app uses the injected URL:
whether it appends full paths (`/api/chat`) or relative paths (`chat`), and
choose the appropriate template variable and route configuration.

### Env injection phase classification

v1 env injection targets a specific phase:

```text
runtime_env:
  Container-start-time environment variables.
  The container receives these as OS env vars before its entrypoint runs.
  Effective for: apps that read env at startup (Dify, Supabase, Appsmith).

entrypoint_template_env:
  Container entrypoint performs template expansion on config files before
  launching the main process.
  Effective for: apps whose entrypoint substitutes env vars into nginx.conf,
  config.yaml, etc. (Dify does this via entrypoint.sh).

build_env (v1 non-goal):
  Variables baked into static assets at image build time (e.g., Next.js
  NEXT_PUBLIC_* vars). These cannot be injected at runtime because they
  are already compiled into JavaScript bundles.
  v1 explicitly does NOT support this class of variables.
```

If a recipe declares `env_inject` for a variable that is actually a build-time
variable, the injection will succeed (env var will be set) but the frontend
code will ignore it. Recipe authors must verify that target variables are
runtime-effective for their specific image.

### Endpoint map (session state, not identity)

After port allocation and router startup, Ato computes:

```
ATO_INGRESS_URL=http://127.0.0.1:<router>/i/<session>/
ATO_SERVICE_WEB_URL=http://127.0.0.1:<router>/i/<session>/web/
ATO_SERVICE_API_URL=http://127.0.0.1:<router>/i/<session>/api/
```

These are stored in the session record. The `ATO_SERVICE_*` names are derived
from route aliases.

---

## 6. Identity Model

The three-domain identity model (`docs/execution-identity.md`) must remain
intact:

| Ingress concept              | Identity domain | Rationale                                                      |
| ---------------------------- | --------------- | -------------------------------------------------------------- |
| Declared route shape         | Declared        | Route names, targets, ports, listed, aliases are manifest      |
| Env var name / template      | Declared        | Template expressions in `env_inject` are manifest declarations |
| `strip_prefix` setting       | Declared        | Routing behavior policy from manifest                          |
| `upstream_path_prefix`       | Declared        | Routing behavior policy from manifest                          |
| `listed` flag                | Declared        | Discoverability policy from manifest                           |
| `root` flag                  | Declared        | Route ownership policy from manifest                           |
| Allocated router host port   | Session         | Dynamic allocation, host-specific                              |
| Session ID in path           | Session         | Ephemeral, changes per run                                     |
| Live proxy PID               | Session         | Runtime process state                                          |
| Resolved URL values          | Session         | Concrete host:port/path, never in identity                     |
| Route target/container port  | Declared        | Part of the manifest-declared service topology                 |

**Invariant:** Changing the `[ingress]` block in `capsule.toml` changes the
`declared_execution_id`. Changing the allocated router port or session ID does
not.

### Env injection identity boundary

Ingress env injection has two layers:

- **Declared template shape**: the env var names and template expressions
  (e.g., `web.CONSOLE_API_URL = "{{ingress.routes.api.url}}"`) are identity
  inputs. They participate in the declared execution identity.
- **Resolved URL values**: the concrete `http://127.0.0.1:<router>/i/<session>/api/`
  strings are session state, excluded from identity.

**The resolved URL must not be included in the canonical env closure used for
execution_id computation.** Instead, the canonical env closure records the
template reference, not the resolved value. This prevents execution identity
from changing between sessions due to different port allocations.

Implementation note: when computing the env hash for identity, the ingress
env variables must either be excluded from the hash entirely (since their
*shape* is already captured by the `env_inject` declaration) or the hash must
use the template string, not the resolved URL.

---

## 7. Security Model

### v1 requirements

- **Bind address**: The router binds **only** to `127.0.0.1` (IPv4 loopback).
  It MUST NOT bind to `0.0.0.0`, `::`, or any non-loopback interface.
- **Host header policy (Option A)**: The router rejects requests where the
  `Host` header does not exactly match `127.0.0.1:<router-port>`. The
  advertised URL is always `127.0.0.1`, and Desktop MUST open this exact
  authority without converting to `localhost` or `::1`. This is the simplest
  and most restrictive policy for v1. If broader hostname support is needed,
  it must be explicitly designed in a future revision.
- **WebSocket**: not supported in v1. Explicit future work. Connections with
  `Connection: Upgrade` are rejected or passed through without upgrade
  handling (implementation choice, documented in router behavior).

### Route scoping

- The router **only** proxies to services declared in the `[ingress.routes]`
  block. No arbitrary upstream discovery or open proxy behavior.
- Undeclared services are unreachable through the ingress proxy, even if their
  host ports are technically allocated.
- The router rejects requests with path prefixes that do not match any declared
  route.

### Session isolation

- Each session's router instance is a separate process with its own port.
- Path-based routing prefixes include the session ID, preventing
  cross-session access even if router ports were somehow shared (they are not).
- No cross-capsule route leakage: a capsule's ingress only routes to its own
  services.

### Localhost CSRF and browser-origin risk

The router binds to `127.0.0.1`, which is reachable from the user's browser.
This creates a class of risk distinct from DNS rebinding:

- A malicious external website can send requests to
  `http://127.0.0.1:<router>/...` via the user's browser (form submissions,
  fetch with `no-cors` mode).
- Same-Origin Policy prevents the external site from *reading* the response,
  but the *side effects* of GET/POST requests are still executed.
- Host header validation does not mitigate this: the browser sends the correct
  `Host: 127.0.0.1:<port>` header.

**v1 mitigation:**

The session route prefix includes the session ID, which acts as a
nonce. However, the session ID alone may not provide sufficient entropy if it
is predictable or discoverable (e.g., visible in `ato ps` output, logged to
terminal). v1 relies on the following combined measures:

1. **Route allowlist**: only declared routes are reachable. Arbitrary paths
   return 404.
2. **Session prefix**: requests must include the correct session ID in the
   path.
3. **Application-layer auth**: for mutating operations, apps should implement
   their own authentication (CSRF tokens, session cookies). This is the
   app's responsibility, not the ingress router's.

**Explicit non-guarantee:** `/i/<session>/...` is not a secret. The session ID
is not a security token. v1 accepts this residual localhost CSRF risk and
relies on application-layer auth for mutating operations. Ato ingress is not
an authentication boundary. If stronger localhost CSRF protection is needed,
future revisions should consider:

- High-entropy route tokens (separate from session ID).
- `Origin` / `Referer` header validation for unsafe methods.
- A local authorization proxy layer.

### Route discoverability (`listed`)

- `listed = true`: the route appears in the session's top-level index
  (`/i/<session>/`) and is the intended user-facing entry point.
- `listed = false`: the route is reachable via its path but hidden from the
  index. Use for internal APIs that the frontend needs but the user does not
  open directly.
- **`listed = false` is not an access-control boundary.** It only controls
  whether the route appears in generated index pages and primary UI surfaces.
  Any client that knows the path can reach it. If access restriction is needed,
  it must be implemented at the application layer (not by the ingress router).

### Secret redaction

- Route diagnostics (`ato ps`, session records) display endpoint URLs but
  **never** include secret values from the launch envelope.
- Env injection templates are resolved before container start; the resulting
  URLs are not logged at info level or above.

### CORS and cookies

- Path-based v1 operates under a single origin (`http://127.0.0.1:<router>`).
- Cookies set by one service are visible to all services on the same origin by
  default. Apps must set explicit `Path` attributes to scope cookies.
- CORS is not an issue for same-origin requests but becomes relevant if a
  service makes cross-origin requests to an external endpoint.

### SSRF risk

A compromised service inside the container network could attempt to reach other
services through the ingress proxy's host port. The router must not rely on
containers being unable to reach host loopback as the primary security
boundary — Docker/Podman provide `host.docker.internal`, gateway host access,
and VM forwarding features that may bypass this assumption.

**Primary boundaries (v1):**

- Route allowlist: only declared upstreams receive traffic.
- Session scoping: paths must include the session prefix.
- Loopback bind: router only listens on `127.0.0.1`.

**Future hardening:**

- Reject requests originating from known container-network IP ranges.
- Validate request source before forwarding.

---

## 8. Validation Rules

The following validation rules are enforced at manifest load time (PR 1) and
again at session start (PR 2). Violations are hard errors, not warnings.

### Route validation

```text
- alias must be unique across all routes in one ingress block.
- alias must be a URL-safe path segment:
  - only lowercase alphanumeric, hyphens, underscores
  - must not contain "/", "..", "%2f", "%5c", or percent-encoded characters
  - must not be empty unless root = true
- target must reference a service that exists in [services].
- port must be declared or exposed by the target service, or explicitly
  allowed by the recipe.
- at most one route may have root = true.
- root = true routes must not set alias. root and alias are mutually exclusive.
- non-root routes must set alias. if alias is omitted for a non-root route,
  the route name (key under [ingress.routes]) is used as the alias.
- if no route has root = true, the session root /i/<session>/ returns a
  generated index page listing all listed routes.
- strip_prefix and upstream_path_prefix must not conflict:
  - upstream_path_prefix is only valid when strip_prefix = true.
  - upstream_path_prefix must start with "/" if present.
  - upstream_path_prefix must not contain "..", traversal sequences, or
    percent-encoded slashes.
```

### Env injection validation

```text
- env_inject target (outer key) must reference a service that exists in
  [services].
- env_inject template value must reference existing route names:
  {{ingress.routes.<name>.url}} — <name> must be a declared route.
- env var names must be valid OS environment variable names (uppercase
  alphanumeric + underscore, no leading digit).
```

### Identity validation

```text
- If env_inject is declared, the canonical env closure for execution_id
  must use the template string, not the resolved URL value.
- Validation must fail at manifest load time if the ingress block would
  produce an ambiguous identity (e.g., duplicate alias, conflicting
  upstream_path_prefix).
```

---

## 9. Lifecycle Model

### Startup sequence

```
1. ato run resolves manifest and lock
2. EndpointAllocator allocates host ports for all services
3. Ingress router starts on a newly allocated port (bound to 127.0.0.1)
4. Router binds routes to upstream host:port targets
5. Env endpoint map is computed from the session-stable ingress URLs
6. Containers start with injected env (if declared)
7. Session record is written with ingress URL and status=running
```

This ordering ensures that containers receive their dependency URLs (e.g.,
`CONSOLE_API_URL`) before their entrypoints execute.

### Session record: ingress state

The session record includes an ingress status object that tracks router health:

```json
{
  "ingress": {
    "status": "running",
    "pid": 12345,
    "base_url": "http://127.0.0.1:42157/i/abc123/",
    "routes": [
      {
        "alias": "web",
        "url": "http://127.0.0.1:42157/i/abc123/web/",
        "listed": true
      },
      {
        "alias": "api",
        "url": "http://127.0.0.1:42157/i/abc123/api/",
        "listed": false
      }
    ]
  }
}
```

**Status values:**

| Status    | Meaning                                           |
| --------- | ------------------------------------------------- |
| `running` | Router process is alive, routes are active        |
| `failed`  | Router crashed or failed to start                 |
| `stopped` | Session teardown in progress or complete          |
| `disabled`| No `[ingress]` block declared for this session    |

### Teardown

- `ato stop <session>` stops the ingress router process alongside all
  containers. Sets `ingress.status = "stopped"`.
- `ato stop --all` stops all routers and all sessions.
- Session record cleanup removes the ingress URL reference.
- Allocated ports return to the OS pool.

### Observability

- `ato ps` shows the ingress endpoint (`ATO_INGRESS_URL`) as the primary URL
  for the session, replacing the raw container host port.
- `ato ps` shows `ingress_status` — if `failed` or `stopped`, the display
  indicates the degraded state.
- `ato ps --verbose` shows individual route URLs.
- Desktop "open app" navigates to the ingress URL, not the container port.
- Desktop shows an error card when `ingress_status = failed` with options to
  restart the session or stop it.

### Failure modes

- **Router port allocation failure**: fail the session start (no fallback to
  raw ports, since that would break the env injection contract).
- **Router crash during session**: detect via PID monitoring, set
  `ingress.status = "failed"`, log the event. Do **not** silently restart —
  the user must explicitly re-run. CLI and Desktop both surface the degraded
  state.
- **Upstream service down**: router returns `502 Bad Gateway` with the service
  name in the response body for diagnostics.

---

## 10. Dify Mapping

Dify is the canonical motivating case. The current `samples/recipes/dify/capsule.toml`
defines six services: `db`, `redis`, `weaviate`, `api`, `worker`, `web`.

### Current state

- `web` reaches HTTP 200 (static assets load).
- Interactive UI is broken because `CONSOLE_API_URL` is empty — the `web`
  container does not know where the `api` service is.
- Upstream Dify uses nginx to route `/` -> web (port 3000) and `/api` -> api
  (port 5001). Without an Ato-managed equivalent, the frontend cannot call
  the backend.

### Validation hypothesis

**Full interactive Dify pass is a validation hypothesis, not a guaranteed
outcome of this ADR.** Path-based routing introduces base-path risk for
apps that assume `/` ownership. Dify must be empirically validated after
PR 2/PR 3 to confirm one of the following:

1. Dify `web` respects a configurable base path and generates
   relative-path URLs — path-based routing works directly.
2. Dify `web` generates absolute-path URLs starting with `/` — path-based
   routing requires `root = true` on the web route, and the app must still
   work under the session prefix.
3. Dify `web` hardcodes absolute paths with no base-path support —
   path-based routing cannot deliver full interactive pass; hostname-based
   routing (v2) is required.

The ADR proceeds with path-based v1 under the assumption that case 1 or 2
applies, but this must be confirmed empirically.

### With local ingress (assumes case 1 or 2)

```toml
[ingress]
mode = "path"

[ingress.routes.web]
target = "web"
port = 3000
listed = true
root = true
strip_prefix = true

[ingress.routes.api]
target = "api"
port = 5001
listed = false
alias = "api"
strip_prefix = true
upstream_path_prefix = "/api"

[ingress.env_inject]
web.CONSOLE_API_URL = "{{ingress.routes.api.url}}"
web.SERVICE_API_URL = "{{ingress.routes.api.url}}"
```

**Path flow for API requests:**

```
Browser:  GET http://127.0.0.1:42157/i/abc123/api/chat/messages
Router:   matches alias="api" (takes priority over root route)
          strips /i/abc123/api/
          prepends upstream_path_prefix="/api"
Forward:  GET http://127.0.0.1:<api-host-port>/api/chat/messages
Backend:  receives /api/chat/messages — matches Dify's nginx /api/* rule
```

**Path flow for web assets (root route):**

```
Browser:  GET http://127.0.0.1:42157/i/abc123/_next/static/chunk.js
Router:   matches root route (web), no alias matched
          strips /i/abc123/
Forward:  GET http://127.0.0.1:<web-host-port>/_next/static/chunk.js
Backend:  receives /_next/static/chunk.js — normal Next.js asset serving
```

**Result (if Dify respects base path or relative URLs):**

1. Router starts, allocates port (e.g., `42157`).
2. `web` container starts with `CONSOLE_API_URL=http://127.0.0.1:42157/i/abc123/api/`.
3. Browser loads `http://127.0.0.1:42157/i/abc123/` (root route).
4. Frontend JavaScript calls `CONSOLE_API_URL` for API requests.
5. Router strips path prefix, prepends `/api`, forwards to the `api` container.
6. Interactive Dify UI is reachable.

### Phase verification for Dify

Dify's `web` image uses an entrypoint (`entrypoint.sh`) that substitutes env
vars into nginx config at container start. `CONSOLE_API_URL` is a
**runtime_env / entrypoint_template_env** variable — it is read at container
start, not baked into the image at build time. This means v1 env injection is
sufficient.

`NEXT_PUBLIC_*` variables are build-time and out of scope, but Dify does not
require them for the API URL use case.

### Storage/files routes

If Dify's `FILES_URL` endpoint is needed:

```toml
[ingress.routes.files]
target = "api"
port = 5001
listed = false
alias = "files"
strip_prefix = true
upstream_path_prefix = "/files"

[ingress.env_inject]
web.FILES_URL = "{{ingress.routes.files.url}}"
```

---

## 11. Open Questions

1. **Base URL compatibility**: How many apps support configurable base paths?
   Dify uses `NEXT_PUBLIC_BASE_PATH` (Next.js), but not all multi-service apps
   do. The root-route mechanism in v1 is a partial mitigation. Empirical
   validation is required per app.

2. **Hostname-based v2**: What is the minimum viable DNS story on macOS and
   Windows? Is `*.localhost` resolution reliable enough in 2026? Should we
   ship a lightweight DNS resolver?

3. **TLS**: Even for localhost, some apps require HTTPS (Secure cookies, OAuth
   callbacks). Can we reuse the existing `bootstrap_tls()` from `binding/proxy.rs`
   for ingress, or does the automated lifecycle need a different approach?

4. **Cookie domain handling**: Single-origin path-based routing means cookies
   from one service leak to others by default. Document the risk and recommend
   explicit `Path` attributes in recipes.

5. **Auth callbacks (OAuth)**: External OAuth providers redirect to a callback
   URL. The ingress URL must be registered with the provider, but it changes
   per session. Is there a viable workaround, or is this a hostname-based v2
   concern?

6. **Route env injection ownership**: Should `env_inject` live in the recipe
   (a policy applied to a specific capsule variant) or in the manifest (part
   of the capsule's declared identity)? Recipes can override manifests, but
   the identity boundary differs.

7. **Localhost CSRF hardening**: Should v1 use high-entropy route tokens
   separate from session IDs? What is the UX cost of longer URLs?

---

## 12. Follow-up Implementation Plan

This ADR is **documentation only**. No runtime code changes are included in
this PR.

### PR 1: Route model + identity **(IN PROGRESS — `feat/ingress-route-model`)**

- [x] Define Rust types for `IngressConfig`, `IngressRoute`, `EnvInjection` in
  `capsule-core` (per the type model in section 5).
- [x] Implement `IngressMode::validate_v1()` that rejects `Host` mode.
- [x] Add `[ingress]` parsing to the manifest loader.
- [x] Implement all validation rules from section 8.
- [x] Integrate ingress declarations into the declared execution identity
  computation. Ensure resolved URL values are excluded from the canonical
  env closure.
- [x] Add round-trip tests for the new types.
- [x] **No proxy, no runtime changes.**

### PR 2: Local path router for OCI sessions **(COMPLETE)**

- [x] Implement session-scoped ingress router in `ato-cli` (new `ingress_router.rs`
  module using axum + reqwest).
- [x] Integrate into the OCI session lifecycle: start router after port allocation
  and container start, before session record write. Stop router during cleanup.
- [x] Add route registration that maps path prefixes to upstream host:port pairs.
- [x] Implement path rewrite contract: `strip_prefix`, `upstream_path_prefix`,
  root route fallback.
- [x] Router binds only to `127.0.0.1`.
- [x] Compute and store the endpoint map in the session record (`OciSessionIngressRecord`).
- [x] Write `ingress` metadata with `mode`, `router_port`, `token`, `primary_url`, `routes`
  in the session record.
- [x] Update `ato ps` to display ingress primary URL as the session endpoint.
- [x] Update `ato ps --json` to include ingress metadata.
- [x] Host header validation (Option A) — accepts `127.0.0.1:<port>` and
  `localhost:<port>`, rejects others with 400.
- [x] SSE / chunked response streaming — currently buffers full body (acceptable
  for v1; streaming hardening is follow-up work due to http-body version
  mismatch between axum 0.7 (http-body 1.0) and hyper 0.14 (http-body 0.4)).
- [x] Trailing-slash redirect (`308`) — `/i/<token>` → `/i/<token>/`,
  `/i/<token>/<alias>` → `/i/<token>/<alias>/`, query string preserved.
  Unknown aliases return 404, not redirect.
- [x] WebSocket: **not supported in v1**. No upgrade handling.
- [x] Validation rules from section 8 are enforced at session start by the
  existing manifest validation (PR 1).
- **Scope:** OCI sessions with `[ingress]` block only.
- **Limitations documented:**
  - Token is high-entropy random (base64url-encoded 32 bytes).
  - Router is a tokio task within `ato run`, not a separate process.
  - `ato stop --all` stops containers, which triggers `ato run` cleanup and
    router shutdown.
  - Response body is buffered (not streamed) due to http-body version
    mismatch. Streaming is follow-up work.

### PR 3: Env endpoint injection + Dify AODD retry

- Implement `env_inject` template resolution using the computed endpoint map.
- Ensure resolved URLs are excluded from identity computation.
- Inject resolved URLs into container environment before start.
- Update the Dify recipe with `[ingress]` and `[ingress.env_inject]` blocks.
- Re-run AODD evaluation against Dify to validate the interactive hypothesis.
- **If Dify fails** due to absolute-path generation: document the specific
  breakage and add hostname-based routing as a dependency for Dify full pass.
- **Scope:** Env injection + Dify recipe update.

### PR 4: Desktop endpoint integration

- Update `ato-desktop` to read the ingress URL from the session record.
- Desktop opens the exact `127.0.0.1:<port>` URL without conversion to
  `localhost` or `::1` (per Host header Option A).
- "Open app" navigates to the ingress endpoint instead of the raw container
  port.
- Display route structure in the Desktop session UI.
- Show error card when `ingress_status = failed` with restart/stop options.
- **Scope:** Desktop shell integration only.

### Validation

- This PR: documentation only. Verify with `git diff --check`.
- Implementation PRs: `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --workspace`.
