# Community Desktop Import — AODD Summary (2026-06-01)

## Result: complete

## Verified

| slug | source | ctoml_id | result |
|---|---|---|---|
| pgweb | `github.com/sosedoff/pgweb` | `ctoml_01ksza4ny27g7g3caspr6p49qk` | ✅ ready |

## Published samples

All 6 samples published to `https://api.ato.run/v1/capsule-tomls`:

| slug | source | ctoml_id |
|---|---|---|
| memos | `usememos/memos` | `ctoml_01ksza4mjs1feg3gwvx9cy35w5` |
| uptime-kuma | `louislam/uptime-kuma` | `ctoml_01ksza4mvjtk8wthttjk3h2zxv` |
| n8n | `n8n-io/n8n` | `ctoml_01ksza4n4knkqms45eaqbp6cc7` |
| blinko | `blinkospace/blinko` | `ctoml_01ksza4ndct49z3qdsvs73p4g1` |
| excalidraw | `excalidraw/excalidraw` | `ctoml_01ksza4np2yrs1mqe7jz10ep1g` |
| pgweb | `sosedoff/pgweb` | `ctoml_01ksza4ny27g7g3caspr6p49qk` |

## CLI verification: pgweb

```
ATO_COMMUNITY_API_URL=https://api.ato.run \
  ato app session start github.com/sosedoff/pgweb \
  --community-toml-id ctoml_01ksza4ny27g7g3caspr6p49qk --json
```

Output:
- status: ready
- session_id: ato-desktop-session-72580
- local_url: http://127.0.0.1:41565/
- stop: { stopped: true }

## Desktop UI path (wired, not GUI-exercised)

- Omnibar: typing `github.com/sosedoff/pgweb` → community candidate appears
- Candidate carries `ctoml_id = "ctoml_01ksza4ny27g7g3caspr6p49qk"`
- Clicking → `LaunchCapsule { handle: "capsule://github.com/sosedoff/pgweb", community_toml_id: Some("ctoml_01ksza4ny27g7g3caspr6p49qk") }`
- → `NavigateToUrl { url: "capsule://github.com/sosedoff/pgweb?ctoml=ctoml_01ksza4ny27g7g3caspr6p49qk" }`
- → `GuestRoute::CapsuleHandle { community_toml_id: Some("ctoml_...") }`
- → `ato app session start github.com/sosedoff/pgweb --community-toml-id ctoml_... --json`

## Failure notes

- memos: `state 'data' requires an explicit persistent binding` — correct behavior; persistent state setup needed before first launch.
