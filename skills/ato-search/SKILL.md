---
name: ato-search
description: Discover apps and tools available through ato.run by natural-language description or structured capability filters. Use when the user asks to find an app, tool, or package from the ato registry by what it does — e.g. "a chat tool that doesn't need the network", "local AI agent", "markdown editor without API keys". Covers capability filters for network, filesystem, side-effects, and secrets requirements.
---

# ato Capsule Search

Find apps and tools in the [ato.run](https://ato.run)
registry by what they do. Two paths are available on the same endpoint;
prefer the GET path whenever you can produce filters from the vocabulary
below.

## When to use this skill

Trigger on any user intent like:

- "find / discover / search for a capsule, tool, app, package"
- "I need something that [capability phrase]"
- "what tools can [verb] without [constraint]"

Examples of in-scope requests:

- "A chat tool that works offline"
- "ネットワーク不要のマークダウンエディタ"
- "Find a capsule that can convert PDF to markdown without an API key"
- "read-only viewer with no side effects"

If the user mentions a specific capsule by `@publisher/slug`, fetch it
directly with `GET /v1/capsules/by/{publisher}/{slug}` instead of searching.

## Preferred path — self-translate, then GET /v1/search

The GET path does not require an LLM translation. Use it when you can extract filters
from the user's NL yourself.

### Capability vocabulary (schema_version: "1")

| Filter            | Values                                         | Semantics                |
|-------------------|------------------------------------------------|--------------------------|
| `network`         | `none`, `egress`, `ingress`, `bidirectional`   | OR within; AND across    |
| `fs_writes`       | `none`, `scratch`, `user-data`, `system`       | OR within; AND across    |
| `side_effects`    | `readonly`, `idempotent`, `mutating`, `destructive` | OR within; AND across |
| `secrets_required`| `true`, `false`                                | boolean                  |

Each filter accepts a comma-separated list. Multiple values on one key
are OR'd; different keys are AND'd together.

### NL → filter mappings (English)

| User says                                         | Filters                                                   |
|---------------------------------------------------|-----------------------------------------------------------|
| "offline" / "no network" / "works without internet" | `network=none`                                           |
| "calls an API" / "online" / "cloud-based"          | `network=egress`                                          |
| "P2P" / "peer-to-peer"                             | `network=bidirectional`                                   |
| "listens on a port" / "server"                     | `network=ingress,bidirectional`                           |
| "no API key" / "no credentials"                    | `secrets_required=false`                                  |
| "read-only" / "viewer" / "no side effects"         | `side_effects=readonly`                                   |
| "idempotent"                                       | `side_effects=idempotent,readonly`                        |
| "can delete files" / "destructive"                 | `side_effects=destructive,mutating`, `fs_writes=user-data,system` |
| "writes temp files only"                           | `fs_writes=scratch`                                       |
| "installer" / "modifies system"                    | `fs_writes=system`, `side_effects=mutating,destructive`   |

### NL → filter mappings (日本語)

| ユーザー発話                               | フィルタ                                                |
|-------------------------------------------|---------------------------------------------------------|
| 「オフライン」「ネットワーク不要」「ネット接続なし」 | `network=none`                                          |
| 「API呼び出し」「オンライン」「クラウド」   | `network=egress`                                        |
| 「P2P」「双方向通信」                     | `network=bidirectional`                                  |
| 「ポートで待ち受ける」「サーバー」         | `network=ingress,bidirectional`                          |
| 「APIキー不要」「鍵不要」                 | `secrets_required=false`                                 |
| 「閲覧のみ」「読み取り専用」「副作用なし」 | `side_effects=readonly`                                  |
| 「冪等」                                  | `side_effects=idempotent,readonly`                       |
| 「削除できる」「破壊的」                  | `side_effects=destructive,mutating`, `fs_writes=user-data,system` |
| 「一時ファイルのみ」                      | `fs_writes=scratch`                                      |
| 「インストーラ」「システム領域を変更」    | `fs_writes=system`, `side_effects=mutating,destructive`  |

### Request

```http
GET https://api.ato.run/v1/search?q=chat&network=none&limit=20
```

Parameters:

- `q` — free-text keywords (slug / name / description match)
- `network`, `fs_writes`, `side_effects` — comma-separated enum values
- `secrets_required` — `true` or `false`
- `category` — e.g. `productivity`, `developer-tools`
- `tags` — repeatable or comma-separated
- `rank` — `popular` (default) or `engaged`
- `limit` — 1..50 (default 20)
- `cursor` — opaque pagination token from a prior response

### Response shape

```json
{
  "schema_version": "1",
  "effective_query": {
    "q": "chat",
    "filters": { "network": ["none"], "fs_writes": null, "side_effects": null, "secrets_required": null },
    "category": null,
    "tags": [],
    "rank": "popular",
    "limit": 20
  },
  "capsules": [
    {
      "id": "...",
      "scoped_id": "@acme/local-chat",
      "slug": "local-chat",
      "name": "Local Chat",
      "description": "An offline chat tool.",
      "category": "productivity",
      "type": "app",
      "publisher": { "handle": "acme", "verified": true, "...": "..." },
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

### Reference vocabulary endpoint

Pin your self-translation against the current schema:

```http
GET https://api.ato.run/v1/search/vocabulary
```

Returns the canonical enum lists plus `schema_version`. Re-fetch it if a
filter value ever produces a 400.

## Fallback path — POST /v1/search with `{nl}`

Use this **only when confident self-translation is not possible**, e.g.
the user's phrasing doesn't map cleanly to the vocabulary or mixes
languages you don't support. The fallback is:

- rate-limited to **30/min/IP anonymous** (300/min with API key)
- **never cached** (`Cache-Control: no-store`)
- billed against server-side LLM budget

### Request

```http
POST https://api.ato.run/v1/search
Content-Type: application/json

{
  "nl": "ネットワーク不要のチャット",
  "lang": "ja",
  "schema_version": "1",
  "filters": { "secrets_required": false },
  "limit": 20
}
```

Optional fields:

- `lang` — BCP-47 hint; server auto-detects otherwise
- `filters` — structured overrides merged with the translation
  (caller's values win per-key)
- `q`, `category`, `tags`, `rank`, `limit`, `cursor` — same as GET

### Response shape

```json
{
  "schema_version": "1",
  "translation": {
    "q": "chat",
    "filters": { "network": ["none"], "fs_writes": null, "side_effects": null, "secrets_required": null },
    "model_version": "nl2filter-prompt-1+anthropic:claude-...",
    "parse_confidence": 0.94,
    "fixture_matches": ["cap-ja-001"],
    "warnings": []
  },
  "effective_query": {
    "q": "chat",
    "filters": { "network": ["none"], "fs_writes": null, "side_effects": null, "secrets_required": false },
    "category": null,
    "tags": [],
    "rank": "popular",
    "limit": 20
  },
  "capsules": [ /* same shape as GET */ ],
  "next_cursor": null
}
```

The `translation` block is always accompanied by executed `capsules` —
the endpoint cannot be used as a standalone NL→JSON translator.

## Always echo the parsed query to the user

Before showing results, summarize the filters that will be applied.
The GET path returns them in `effective_query`; the POST path returns
both `translation` (LLM output) and `effective_query` (post-merge).

Example:

> Searching for **chat** capsules with **network: none**. Found 12 results.

If `parse_confidence < 0.7` on the POST path, ask the user to confirm
the translation before presenting results.

## Interpreting results

- `publisher.verified: true` → GitHub-verified publisher (stronger trust
  signal); prefer these in ambiguous results.
- `capabilities.*: null` → not declared by the author; treat as unknown,
  not as "safe".
- `vouches_count` → community attestations; higher is better.
- Empty `capsules` array → suggest the user file an `ato request` for
  the capability, or broaden their filters.

## Error handling

| Status | Meaning                              | Recovery                                                 |
|--------|--------------------------------------|----------------------------------------------------------|
| 400    | Bad input (unknown enum, bad JSON)   | Read `error.field` and `error.allowed`; fix and retry    |
| 422    | POST only — LLM output failed schema | Fall back to GET with keyword-only search               |
| 429    | Rate-limited                         | Respect `retry_at`; use the GET path instead            |
| 503    | POST only — LLM upstream unavailable | Fall back to GET with keyword-only search               |

The server never echoes raw LLM text on 422, so clients cannot rely on
it for debugging.

## Progressive disclosure

Additional references are loaded on demand:

- `examples.jsonl` — 10 representative NL→filter pairs for one-shot
  prompting. Loaded when you need fresh exemplars.
- `endpoints.md` — full API reference including auth, pagination, and
  error bodies. Loaded when you need more detail than this file.

Both live alongside `SKILL.md` in the skill package.
