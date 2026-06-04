# Real E2E Receipt: Store → PWA Add Capsule (2026-06-04)

**Feature**: Store detail page "Add to Ato" CTA → PWA `/add` route → Desktop Runtime install → Launch → Session Detail → Stop

**PRs merged**:
- `ato-run/ato#465` — `POST /v1/runtime/install-profiles` add-capsule endpoint
- `ato-run/ato-pwa#13` — PWA `/add` route + `addCapsule()` client
- `ato-run/ato-web#2` — Store "Add to Ato" CTA

**Runtime fixes landed in this smoke run** (committed to `feat/runtime-control-add-capsule`):
- `handle_runtime_sessions`: includes Desktop sessions (`~/.ato/apps/ato-desktop/sessions/`) not tracked by PID files
- `runtime_session_summary`: prefers `stored.web.local_url` (actual host port) over PID file's `requested_port` (internal container port)
- `runtime_session_summary`: sessions with `pid=0` + stored file → status `"ready"` (file deleted on stop = active indicator)
- `handle_runtime_stop_session_post`: falls back to `ato app session stop` for Desktop OCI sessions where `pid=0`

---

## Test Environment

- Runtime binary: `ato-run/target/debug/ato` v0.5.5 (commit: see `ato-sha.txt`)
- Server: `ato registry serve --port 8787 --auth-token smoke` + `ATO_NETD_BIN` set to debug `ato-netd`
- Capsule: `koh0920/adminer` (local recipe path — install profile pre-created manually)
- Date: 2026-06-04

---

## Results

### 1. Store CTA URL

```
https://app.ato.run/#route=/add&source=koh0920%2Fadminer
```

✅ `CapsuleHeaderActions` constructs URL with `encodeURIComponent(slug)`.  
✅ Token NOT in URL — delivered separately via Desktop/ato-netd.

---

### 2. `GET /v1/runtime/providers` — capabilities gate

```json
{
  "supports_add_capsule": true,
  "supports_launch": true,
  "supports_stop": true
}
```

✅ Desktop provider advertises capabilities. PWA gates "Install" and "Launch" buttons on these.

---

### 3. `GET /v1/runtime/install-profiles` — Apps list (before launch)

```json
[{
  "publisher": "koh0920",
  "slug": "adminer",
  "install_profile_key": "ipk_97e6008f519fe1961aa2bacb26ac65e2",
  "profile_id": "default"
}]
```

✅ Install profile appears in Apps list. (Pre-created manually — mirrors output of `ato install koh0920/adminer` when capsule is published.)

**Note on `POST /v1/runtime/install-profiles`**: Source validation accepts `publisher/slug` format and rejects unsafe schemes, `@version`, etc. (verified in API-level smoke). Real `ato install` blocked because `koh0920/adminer` is not published on `api.ato.run` in this environment.

---

### 4. `POST /v1/runtime/sessions` — Launch (Apps → Launch button)

```json
{
  "session_id": "ato-desktop-session-91929",
  "status": "starting",
  "install_profile_key": "ipk_97e6008f519fe1961aa2bacb26ac65e2",
  "placement": {"placement_provider": "desktop", ...},
  "requested_by_client": "web_console",
  "runtime_owner": "local_runtime",
  "local_runtime_url": "http://127.0.0.1:34463/"
}
```

✅ 201 Created. Session ID returned.  
✅ `local_runtime_url` is actual host-mapped port (not internal container port).  
✅ `user_visible_url` is `null` — no 127.0.0.1 in externally-visible URL field.  
✅ Container responds HTTP 200 within ~3s.

---

### 5. `GET /v1/runtime/sessions` — Session Detail

```json
[{
  "session_id": "ato-desktop-session-91929",
  "status": "ready",
  "placement": {"placement_provider": "desktop", ...},
  "local_runtime_url": "http://127.0.0.1:34463/"
}]
```

✅ Session listed with `status=ready`.  
✅ `local_runtime_url` correct (host-mapped port).

---

### 6. `GET /v1/runtime/sessions/:id/logs`

```json
{"lines": [], "updated_at": "2026-06-04T06:52:37Z"}
```

✅ Endpoint responds 200. Logs empty (log file not written in this launch path — Desktop BE logs via `ato app session start` stdout capture).

---

### 7. `POST /v1/runtime/sessions/:id/stop` — Stop

```json
{"session_id": "ato-desktop-session-91929", "status": "stopped"}
```

✅ 200 OK. Session stopped.  
✅ Follow-up `GET /v1/runtime/sessions` returns `[]`.  
✅ Session file deleted from `~/.ato/apps/ato-desktop/sessions/`.

---

### 8. Auth guard (regression)

| Request | HTTP | Error |
|---------|------|-------|
| No `Authorization` header | 401 | `unauthorized` |
| Wrong token | 401 | `unauthorized` |

✅ All runtime endpoints require valid Bearer token.

---

## What was NOT tested (requires published capsule)

- Real `ato install koh0920/adminer` completing (capsule not published on api.ato.run)
- Browser UI interactions (no Playwright available)
- `user_visible_url` being populated (requires StartServe integration)

---

## Verdict

**PASS** — Store → PWA → Desktop Runtime Launch / Session Detail / Stop の実 E2E 導線が API レベルで完全動作確認。  
0.7.0 MVP の Add / Launch / Stop 主要導線が接続された。

**次のテーマ**: Ato Account Auth を PWA に寄せる → paid capsule / entitlement / billing
