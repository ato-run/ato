# ato-search API Reference

Base URL: `https://api.ato.run`
Schema version: `1` (always pin requests against this value)

All responses include `schema_version` so callers can detect server
changes without side-channel coordination. On any mismatch, refetch
[`/v1/search/vocabulary`](#get-v1searchvocabulary) and re-verify the
enum values you're using.

---

## GET /v1/search

Structured capability-filtered search without an LLM translation. This is the preferred path.

### Query parameters

| Param               | Type                    | Default    | Notes |
|---------------------|-------------------------|------------|-------|
| `q`                 | string                  | ""         | Keywords matched against slug / name / description |
| `network`           | comma-separated enum    | —          | `none`, `egress`, `ingress`, `bidirectional` |
| `fs_writes`         | comma-separated enum    | —          | `none`, `scratch`, `user-data`, `system` |
| `side_effects`      | comma-separated enum    | —          | `readonly`, `idempotent`, `mutating`, `destructive` |
| `secrets_required`  | boolean                 | —          | `true` / `false` |
| `category`          | string                  | —          | e.g. `productivity`, `developer-tools` |
| `tags`              | repeatable or CSV       | —          | AND across tags |
| `rank`              | `popular` \| `engaged`  | `popular`  | `engaged` weights recent interaction |
| `limit`             | integer                 | `20`       | 1..50 |
| `cursor`            | opaque string           | —          | From prior `next_cursor` |

Filter semantics: **OR within one key, AND across keys**. `network=none,egress`
matches capsules whose `network` is `none` or `egress`; combined with
`fs_writes=user-data` both conditions must hold.

### Response body

```jsonc
{
  "schema_version": "1",
  "effective_query": {
    "q": "chat",
    "filters": {
      "network": ["none"],
      "fs_writes": null,
      "side_effects": null,
      "secrets_required": null
    },
    "category": null,
    "tags": [],
    "rank": "popular",
    "limit": 20
  },
  "capsules": [
    {
      "id": "01H...",
      "scoped_id": "@acme/local-chat",
      "slug": "local-chat",
      "name": "Local Chat",
      "description": "An offline chat tool.",
      "category": "productivity",
      "type": "app",
      "publisher": {
        "handle": "acme",
        "display_name": "Acme, Inc.",
        "github_id": 42,
        "author_did": "did:key:...",
        "verified": true
      },
      "latest_version": "1.2.0",
      "downloads": 1234,
      "vouches_count": 5,
      "capabilities": {
        "network": "none",
        "fs_writes": "scratch",
        "side_effects": "readonly",
        "secrets_required": false
      },
      "created_at": "2026-03-01T00:00:00Z",
      "updated_at": "2026-04-18T00:00:00Z"
    }
  ],
  "next_cursor": null
}
```

### Caching

- The current API code sets `Cache-Control: private, no-store` for both anonymous and authenticated responses.

### Rate limits

Rate limits depend on the deployed API configuration; honor a `429` response.

### Error responses

| Status | Body                                                                 |
|--------|----------------------------------------------------------------------|
| 400    | `{ "error": "...", "field": "network", "allowed": [...], "schema_version": "1" }` |

---

## POST /v1/search

Natural-language input variant. **Couples LLM translation to search
execution** — you always get `translation` and `capsules` together.

### Request body

```jsonc
{
  "nl": "ネットワーク不要のチャット",
  "lang": "ja",                             // optional BCP-47 hint
  "schema_version": "1",                    // optional; rejected if != server's
  "filters": { "secrets_required": false }, // optional structured overrides
  "q": "chat",                              // optional keyword override
  "category": "productivity",               // optional
  "tags": ["markdown"],                     // optional
  "rank": "popular",                        // optional; same as GET
  "limit": 20,                              // optional; same as GET
  "cursor": null                            // optional; same as GET
}
```

- `nl` is required and capped at 500 characters.
- `filters` is merged per-key with the LLM's translation; the caller's
  values win. Use this to pin a filter without restating the whole
  translation.
- All other fields override the corresponding field in the translation
  output if present.

### Response body

```jsonc
{
  "schema_version": "1",
  "translation": {
    "q": "chat",
    "filters": {
      "network": ["none"],
      "fs_writes": null,
      "side_effects": null,
      "secrets_required": null
    },
    "model_version": "nl2filter-prompt-1+anthropic:claude-...",
    "parse_confidence": 0.94,
    "fixture_matches": ["cap-ja-001"],
    "warnings": []
  },
  "effective_query": {
    "q": "chat",
    "filters": {
      "network": ["none"],
      "fs_writes": null,
      "side_effects": null,
      "secrets_required": false
    },
    "category": null,
    "tags": [],
    "rank": "popular",
    "limit": 20
  },
  "capsules": [ /* same shape as GET */ ],
  "next_cursor": null
}
```

### Caching

- `Cache-Control: private, no-store` on every response. The CDN does
  not cache; the server keeps an internal LLM-translation cache keyed
  by `sha256(nl + schema_version + model_version)` that the client
  never sees.

### Rate limits

- 30 requests / minute / IP (anonymous)
- 300 requests / minute / user (authenticated)
- Response headers: `X-RateLimit-Remaining`, `X-RateLimit-Reset`

### Error responses

| Status | Reason                                         | Body                                                   |
|--------|------------------------------------------------|--------------------------------------------------------|
| 400    | Invalid JSON, missing `nl`, schema mismatch, invalid filter | `{ "error": "...", "field": "...", "allowed": [...] }` |
| 422    | LLM output failed schema validation            | `{ "error": "translation_invalid", "reason": "...", "field": "..." }` |
| 429    | Rate-limited                                   | `{ "error": "rate_limited", "retry_at": <epoch_s> }`  |
| 503    | LLM upstream unavailable                       | `{ "error": "translation_unavailable" }`              |

Raw LLM output is never echoed in 422 bodies — the server guarantees
that clients cannot exfiltrate unvalidated model text through this
endpoint.

---

## GET /v1/search/vocabulary

Returns the canonical enum lists that any 400 will reference. Cached
like `GET /v1/search` (anonymous path).

```jsonc
{
  "schema_version": "1",
  "network": ["none", "egress", "ingress", "bidirectional"],
  "fs_writes": ["none", "scratch", "user-data", "system"],
  "side_effects": ["readonly", "idempotent", "mutating", "destructive"],
  "secrets_required": { "type": "boolean" }
}
```

---

## Field reference

### `capabilities.*`

Declared by the capsule author in `capsule.toml` and locked into the
release manifest. `null` means the author did not declare the field —
treat it as unknown rather than as a safe default.

### `publisher.verified`

`true` when the publisher's GitHub identity has been verified against
the registry's attestation workflow. Verified publishers appear higher
in `rank=popular` tiebreaks.

### `next_cursor`

Opaque. Pass verbatim as `cursor` on the next request to continue
pagination. `null` means no more results.

---

## Conformance

Implementations of this skill MUST:

1. Pin `schema_version` to `"1"` on every request.
2. Prefer the GET path whenever the NL can be translated from the
   vocabulary above.
3. Echo the effective filters to the user before displaying results.
4. Respect `X-RateLimit-Remaining` / `Retry-After` on 429.
5. Fall back to GET with keyword-only `q=` on 422 / 503 from POST.
