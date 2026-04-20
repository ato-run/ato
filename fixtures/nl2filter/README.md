# NL2Filter fixtures

Gold corpus of natural-language search queries paired with the structured
filter payload they should translate into. Consumed by three places:

1. **Server-side prompt** (`apps/ato-registry-api/src/search/nl_prompt.md`) —
   a curated subset (~15 pairs) is embedded directly as few-shot examples.
2. **`SKILL.md` inline examples** (`apps/ato-search-skill/examples.jsonl`) —
   ~10 pairs are mounted as progressive-disclosure context for agents doing
   client-side self-translation.
3. **Eval harness** (`apps/ato-registry-api/bin/nl-eval`) — regression tests
   measure exact-match filter accuracy on both files.

## Files

- `gold.jsonl` — 100 training pairs (50 ja + 50 en). May appear in prompts
  and SKILL.md.
- `gold-eval.jsonl` — 20 held-out pairs (10 ja + 10 en). **Never** used in
  prompts or SKILL.md; only the harness reads them. Corruption of this
  boundary invalidates every eval result downstream.

## Entry schema

```jsonc
{
  "lang": "ja" | "en",           // source language
  "nl": "...",                   // user-facing NL query
  "q": "...",                    // expected keyword tokens (empty if none)
  "filters": {                   // expected capability filter payload
    "network": ["none", ...],    // enum values; OR within a key
    "fs_writes": [...],
    "side_effects": [...],
    "secrets_required": true | false
  },
  "category": "capability" | "runtime" | "domain" | "ambiguous" | "adversarial",
  "notes": "..."                 // one-line rationale for the mapping
}
```

All enum values must validate against
`apps/ato-cli/core/schema/capabilities.schema.json`.

## Distribution (`gold.jsonl`)

| Category    | Count | Purpose                                            |
| ----------- | ----- | -------------------------------------------------- |
| capability  | 40    | Direct network/fs/side-effects/secrets asks        |
| runtime     | 15    | Runtime-hint phrasing ("WASM", "python", "native") |
| domain      | 15    | Domain words anchored by sample capsule kinds      |
| ambiguous   | 15    | Plausible multiple readings — filters must be safe |
| adversarial | 15    | Prompt-injection / schema-escape attempts          |

## Versioning

Entries are append-only. Removing or changing an existing entry breaks
eval-result continuity — instead, add a replacement and leave a trailing
`"deprecated": true` field on the obsolete one (the harness ignores those).
