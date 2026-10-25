# Separately approved functional HTTP bindings

Status: Draft. These are Adapter interactions for an approved functional acceptance plan, not a new Semantic Core primitive or a change to Contract K. Fixed socket fixtures are implementation checks; real OSS, agent, Runtime Run, persistence and restart acceptance remain separate measurements.

## Scope and compatibility

`RuntimePortOperation` remains the common typed declaration for the existing HTTP Adapter. The frozen exploration catalog supports its original GET/POST, string `json_bindings`, status guard and accepted-status profile. `legacy_exploration_supported()` gates Source authoring, BoundDerivation identity and ordinary D execution. New methods and private extensions are accepted only through `FunctionalAcceptanceV1`, after independent plan authorization against the approved Runtime ceiling. They do not rewrite D/K, grant ordinary Run permission from an exploration PASS, or advertise a new exploration capability.

All new optional maps, flags and lists are omitted when empty/default. Existing JSON/JCS D bytes and receipt interpretation remain unchanged. Legacy static request-path validation and `Accept: application/json` behavior are preserved. GET continues to reject a JSON body. Unknown fields, literal headers, credential values and arbitrary nested JSON/template evaluation are rejected.

There is one scoped invocation implementation in `ato-adapter-http::bound_request::Session`, reused by the original `invoke` entry and Runtime functional acceptance. There is no second budget, receipt, request retry or Contract validator.

## Typed declarations

`RequestTemplate` adds optional fields:

| Field | Meaning |
| --- | --- |
| `header_bindings` | Lowercase header name to `{binding, encoding: direct or bearer}` metadata. Raw values and routing/framing/proxy/Cookie headers are forbidden. |
| `path_bindings` | A whole `{slot}` path segment to a private binding. Rendered segments are nonempty ASCII letters/digits/dot/underscore/hyphen, at most 128 bytes; dot/dot-dot, slash, query, percent encoding and traversal are rejected. |
| `cookie_bindings` | Cookie name to private binding metadata; requires `cookies: true`. It replaces that name with a host-only Path=/ cookie inside this attempt/Port jar. Raw Cookie headers remain forbidden. |
| `json_types` | A declared JSON field to string/boolean/integer. Initial input defaults to the legacy string behavior. Boolean/integer inputs are strictly parsed; integer is i64. A captured scalar retains its type and cannot be reinterpreted by a later explicit type. |
| `cookies` | Explicit opt-in for a private host-only response cookie jar. |

PUT, PATCH and DELETE are explicit methods in this profile. No method is inferred from app identity. Header/path/cookie maps have at most eight entries; JSON bindings have at most 32. Private headers have at most 4096 bytes each, cookie values 2048; control characters and injection are rejected before dispatch.

`response_bindings` contains at most eight captures per operation and 16 across a plan. Each has `binding`, `scalar`, `max_bytes` (1–4096) and exactly one selector:

* `json_pointer`: a bounded public JSON pointer to a string/boolean/integer scalar.
* `html_text_id`: an exact public element ID, string only. UTF-8 text/html is parsed by the HTML5 parser; any parser error, multiple matching IDs, child elements, executable/inert script-style-template-noscript content, empty text or excess bytes is rejected. Leaf text is entity-decoded and trimmed; no selector language, DOM serialization, scripts or external resources are executed. Accept is text/html for this typed selection only.
* `json_object_key`: `{pointer, where_pointer, binding}`. `pointer` may be empty for the root object. At most 128 entries are scanned; the value at each `where_pointer` is compared with the private expected binding. Exactly one matching entry is required. Its key is captured as a string with the same single-segment ASCII restriction and a maximum of min(max_bytes, 128). No match, duplicate match or ambiguous JSON members are rejected.

JSON bodies are parsed by the same recursive strict scalar/object parser for captures and checks. Duplicate members at every nesting level are rejected rather than being overwritten by `serde_json::Value`.

`response_checks` has at most eight entries. Each declares `binding`, optional `scalar`, and exactly one `json_pointer` or lowercase `header_name`. JSON checks compare against the private expected scalar. Header checks are string-only, require one response header value, and reject duplicate values. Only selector/binding metadata and a matched boolean become evidence. A Location check compares the private value and never follows the redirect.

An operation may combine JSON captures with JSON/header checks, or HTML captures with header checks. It cannot mix JSON and HTML body selectors. Header-only checks do not require or expose a response body.

## Authority and private lifecycle

Every request binding, equality binding and object-key predicate must be either a preceding capture on the same logical Port or a current declared Runtime input with exact resource, Execute operation, Runtime phase and artifact_embedding=false. Each operation requires explicit HTTP Execute authority and a Port bound to a process serve step. Captures cannot shadow inputs or other captures, refer forward, cross Ports or become agent variable requirements. Initial grant absence/ambiguity is checked before dispatch.

A fresh nonserializable Session is allocated for each attempt and each assigned logical Port. Physical endpoints are fixed nonzero loopback addresses. Captures and cookies exist only in that Session and disappear on completion/failure; they are not grants, artifact contents, Source edits, D identity or receipt values. Runtime inputs are resolved only from the current redeemed grant set. No store search or fallback resolves a missing binding.

Cookies are host-only; Domain attributes are rejected. Path scope, Secure exclusion on HTTP, Max-Age/Expires expiry and explicit removal are enforced. Duplicate cookie attributes, invalid characters, more than 16 cookies or more than 8192 bytes are rejected. A new attempt/Port does not inherit the jar.

Request observations contain the original declaration template, not the rendered private path/body/headers/cookies. Response observations contain status and declared equality metadata/boolean only. Response bodies, headers, cookies, captured keys, expected values, endpoints and private grant references are absent from public observations and generic errors. Session/PrivateScalar Debug is redacted. Private request/buffer/scalar strings use cleanup/zeroization where owned; this is not a proof of nonexposure through every external orchestrator or application log.

## Bounds, failure and recovery

A functional plan retains the existing maximum of four operations. Authority ceilings, original deadlines and Runtime attempt accounting are unchanged. Network reads/writes use the remaining frozen execution allowance; response headers are bounded to 16 KiB and bodies to 64 KiB. JSON framing rejects duplicate Content-Length/Content-Type, conflicting Content-Length/Transfer-Encoding, unsupported transfer coding, oversized bodies and unsupported trailers.

There is no HTTP redirect follow, endpoint discovery, external egress, query addition, host execution or automatic replay after uncertain mutation delivery. Missing/malformed/ambiguous capture, private equality failure, rejected status or original deadline ends the interaction through the existing Runtime failure path. A later operation cannot run with an unresolved preceding capture. Application state restoration/restart and final receipt/ACK/cleanup still require their own saved evidence.

## Validation boundary

Implementation fixtures cover old canonical request bytes, typed methods and true boolean JSON, exact authority/phase/Port/embedding, prior capture scope, literal/injection refusal, isolated cookies/expiry, strict JSON duplicates, HTML ambiguity/parser errors, object-key cardinality, private Location equality without follow, bounded framing and no replay after response loss. Runtime fixtures exercise four separately approved operations while preserving the original DerivationRef. They do not establish real OSS or native-agent acceptance.

HTML parser reference: [scraper Html](https://docs.rs/scraper/0.27.0/scraper/html/struct.Html.html), backed by html5ever. Dependencies are pinned by Cargo.lock; arbitrary CSS selection is not exposed by this protocol.
