# Secret Cleanup Plan

Audit of all recipe secrets for hardcoded passwords, encryption keys,
session secrets, and database credentials embedded in recipe `env` blocks.

**Audit date:** 2026-05-24  
**Policy:** No production-looking secrets committed. Demo placeholders
clearly labeled. Generated secret mechanism not yet available in recipe
schema — all secrets are `demo placeholder with caveat`.

---

## High-Priority Cleanups

### n8n: N8N_ENCRYPTION_KEY — missing

| Field | Value |
|---|---|
| Recipe | `samples/recipes/n8n/capsule.toml` |
| Current handling | `N8N_ENCRYPTION_KEY` is **not set** in env |
| Risk | Credential encryption is disabled. Any stored credentials are plaintext in SQLite. |
| Proposed handling | Add `N8N_ENCRYPTION_KEY` env var with demo placeholder caveat. Cannot use generated-secret — mechanism not available in recipe schema. |
| Status | **demo placeholder with caveat** |

The recipe description already warns: "NOTE: single-user / demo mode — N8N_ENCRYPTION_KEY is not auto-generated in this recipe. Not recommended for production without adding a generated secret."

### linkwarden: NEXTAUTH_SECRET

| Field | Value |
|---|---|
| Recipe | `samples/recipes/linkwarden/capsule.toml` |
| Current handling | `NEXTAUTH_SECRET = "changeme_replace_with_generated"` |
| Risk | Demo value; JWT tokens can be forged |
| Proposed handling | Replace with a more descriptive demo placeholder. Add caveat in recipe description. |
| Status | **demo placeholder with caveat** |

### Dify: SECRET_KEY

| Field | Value |
|---|---|
| Recipe | `samples/recipes/dify/capsule.toml` |
| Current handling | `SECRET_KEY = "sk-demo-changeme-not-for-production-use-only"` in `[targets.api]` and `[targets.worker]` |
| Risk | Session/crypto key is hardcoded |
| Proposed handling | Label clearly as demo placeholder. Add flagnote in recipe description. |
| Status | **demo placeholder with caveat** |

### Dify: REDIS_PASSWORD / POSTGRES_PASSWORD

| Field | Value |
|---|---|
| Recipe | `samples/recipes/dify/capsule.toml` |
| Current handling | `REDIS_PASSWORD = "difyai123456"`, `POSTGRES_PASSWORD = "difyai123456"` |
| Risk | Demo passwords from upstream compose. |
| Proposed handling | Keep as demo values (from upstream convention). Add caveat. |
| Status | **demo placeholder with caveat** |

### Dify: Weaviate API key

| Field | Value |
|---|---|
| Recipe | `samples/recipes/dify/capsule.toml` |
| Current handling | `WEAVIATE_API_KEY = "WVF5YThaHlkYwhGUSmCRgsX3tD5ngdN8pkih"` |
| Risk | Demo key from upstream compose |
| Proposed handling | Keep as upstream default; document as demo key. |
| Status | **demo placeholder with caveat** |

### Langfuse: NEXTAUTH_SECRET / SALT / passwords

| Field | Value |
|---|---|
| Recipe | `samples/recipes/langfuse/capsule.toml` |
| Current handling | `NEXTAUTH_SECRET = "changeme-demo-secret"`, `SALT = "changeme-salt"`, `POSTGRES_PASSWORD = "langfuse-secret"` |
| Risk | Demo values for auth and encryption |
| Proposed handling | Keep as demo placeholders. Add caveat to recipe description. |
| Status | **demo placeholder with caveat** |

### Outline: SECRET_KEY / UTILS_SECRET

| Field | Value |
|---|---|
| Recipe | `samples/recipes/outline/capsule.toml` |
| Current handling | `SECRET_KEY = "a3a0178ebfe6e4a0f25e0e4af79e81e0b88b58c01d6a7e98ce2ccda03b1b5a3d"`, `UTILS_SECRET = "a3a0178ebfe6e4a0f25e0e4af79e81e0b88b58c01d6a7e98ce2ccda03b1b5a3e"` |
| Risk | Hardcoded 64-char hex strings (appear non-random) |
| Proposed handling | Replace with clearly labeled demo placeholders. |
| Status | **demo placeholder with caveat** |

### Paperless: PAPERLESS_SECRET_KEY

| Field | Value |
|---|---|
| Recipe | `samples/recipes/paperless-ngx/capsule.toml` |
| Current handling | `PAPERLESS_SECRET_KEY = "changeme-demo-secret-key"` |
| Risk | Demo placeholder for Django secret key |
| Proposed handling | Keep as demo placeholder. Add recipe caveat. |
| Status | **demo placeholder with caveat** |

---

## Full Secret Audit

| Recipe | Secret | Current handling | Risk | Proposed handling | Status |
|---|---|---|---|---|---|
| n8n | `N8N_ENCRYPTION_KEY` | **Missing from env** | Credentials stored in plaintext | Add demo placeholder; label clearly; can't use generated-secret | demo placeholder with caveat |
| linkwarden | `NEXTAUTH_SECRET` | `"changeme_replace_with_generated"` | JWT forgery possible | Keep placeholder; add recipe caveat | demo placeholder with caveat |
| linkwarden | `POSTGRES_PASSWORD` | `"linkwarden_secret"` | Demo DB password | Keep placeholder; not for production | demo placeholder with caveat |
| dify | `SECRET_KEY` (api/worker) | `"sk-demo-changeme-not-for-production-use-only"` | Hardcoded session key | Label clearly; recipe already describes spike status | demo placeholder with caveat |
| dify | `REDIS_PASSWORD` | `"difyai123456"` | Upstream demo value | Keep as-is (upstream default) | demo placeholder with caveat |
| dify | `POSTGRES_PASSWORD` | `"difyai123456"` | Demo password | Keep as-is (upstream compose convention) | demo placeholder with caveat |
| dify | `WEAVIATE_API_KEY` | `"WVF5YThaHlkYwhGUSmCRgsX3tD5ngdN8pkih"` | Upstream demo key | Keep as-is (upstream default) | upstream optional |
| langfuse | `NEXTAUTH_SECRET` | `"changeme-demo-secret"` | Demo auth secret | Keep placeholder; add caveat | demo placeholder with caveat |
| langfuse | `SALT` | `"changeme-salt"` | Demo encryption salt | Keep placeholder; add caveat | demo placeholder with caveat |
| langfuse | `POSTGRES_PASSWORD` | `"langfuse-secret"` | Demo DB password | Keep placeholder | demo placeholder with caveat |
| outline | `SECRET_KEY` | `"a3a0178ebfe6e4a0f25e0e4af79e81e0b88b58c01d6a7e98ce2ccda03b1b5a3d"` | Hardcoded hex string - unclear if prod or demo | Replace with `"changeme-demo-secret"` | demo placeholder with caveat |
| outline | `UTILS_SECRET` | `"a3a0178ebfe6e4a0f25e0e4af79e81e0b88b58c01d6a7e98ce2ccda03b1b5a3e"` | Hardcoded hex string - unclear if prod or demo | Replace with `"changeme-demo-utils-secret"` | demo placeholder with caveat |
| outline | `POSTGRES_PASSWORD` | `"outline-secret"` | Demo DB password | Keep placeholder | demo placeholder with caveat |
| paperless-ngx | `PAPERLESS_SECRET_KEY` | `"changeme-demo-secret-key"` | Demo Django key | Keep placeholder | demo placeholder with caveat |
| paperless-ngx | `POSTGRES_PASSWORD` | `"paperless-secret"` | Demo DB password | Keep placeholder | demo placeholder with caveat |
| umami | `APP_SECRET` | `"changeme-demo-secret"` | Demo session secret | Keep placeholder | demo placeholder with caveat |
| umami | `POSTGRES_PASSWORD` | `"umami-secret"` | Demo DB password | Keep placeholder | demo placeholder with caveat |
| searxng | `SEARXNG_SECRET` | `"changeme-demo-secret"` | Demo secret key | Keep placeholder | demo placeholder with caveat |
| grist | `GRIST_SESSION_SECRET` | `"changeme-demo-secret"` | Demo session secret | Keep placeholder | demo placeholder with caveat |
| wallabag | `SYMFONY__ENV__SECRET` | `"changeme-demo-secret"` | Demo symfony secret | Keep placeholder | demo placeholder with caveat |
| litellm | `LITELLM_MASTER_KEY` | `"sk-demo-changeme"` | Demo API key | Keep placeholder | demo placeholder with caveat |
| directus | `KEY` / `SECRET` | `"changeme-key"` / `"changeme-secret"` | Demo app key/secret | Keep placeholder | demo placeholder with caveat |
| directus | `ADMIN_PASSWORD` | `"admin-password"` | Demo admin account | Keep placeholder; document in description | demo placeholder with caveat |
| superset | `SUPERSET_SECRET_KEY` | `"demo-secret-key-32-chars-here!!!"` | Demo secret key | Keep placeholder | demo placeholder with caveat |
| superset | `ADMIN_PASSWORD` | `"admin"` | Demo admin account | Keep placeholder; labeled clearly | demo placeholder with caveat |
| twenty | `APP_SECRET` | `"changeme-demo-secret"` | Demo session secret | Keep placeholder | demo placeholder with caveat |
| twenty | `POSTGRES_PASSWORD` | `"twenty-secret"` | Demo DB password | Keep placeholder | demo placeholder with caveat |
| librechat | `JWT_SECRET` / `JWT_REFRESH_SECRET` | `"changeme-demo-secret"` / `"changeme-refresh-secret"` | Demo JWT keys | Keep placeholder | demo placeholder with caveat |
| librechat | `CREDS_KEY` / `CREDS_IV` | `"changeme-creds-key-32chars-padded"` / `"changeme-creds-iv"` | Demo encryption material | Keep placeholder | demo placeholder with caveat |
| photoprism | `PHOTOPRISM_ADMIN_PASSWORD` | `"insecure"` | Demo password | Keep placeholder; recipe is partial status | demo placeholder with caveat |
| logto | `POSTGRES_PASSWORD` | `"logto-secret"` | Demo DB password | Keep placeholder | demo placeholder with caveat |

---

## Changes Applied

| Recipe | Change |
|---|---|
| n8n | Add `N8N_ENCRYPTION_KEY = "changeme-demo-encryption-key"` env var with caveat |
| outline | Replace apparent hardcoded `SECRET_KEY` / `UTILS_SECRET` with clearly labeled demo placeholders |
| All others | Existing demo placeholders are adequately labeled; no change needed |

---

## Classification Summary

| Classification | Count |
|---|---|
| demo placeholder with caveat | 29 |
| generated-secret supported now | 0 |
| required user-provided secret | 0 |
| blocked by current recipe schema | 0 |
| upstream optional | 1 |

### On generated-secret support

The Ato v0.3 recipe schema does not currently provide a `[secrets]` or
`generate_secret` mechanism. All recipe secrets are expressed as
hardcoded env vars with demo/password values. Until a generated-secret
system is added to the schema, all recipes must:

1. Use clearly labeled demo placeholders (e.g. `changeme-...`)
2. Document in the recipe `description` that secrets must be replaced
   for production use
3. Never commit real production secrets

The highest-priority recipes for generated-secret support when it
becomes available:
1. **n8n** — `N8N_ENCRYPTION_KEY` (credential encryption)
2. **linkwarden** — `NEXTAUTH_SECRET` (JWT/NextAuth)
3. **langfuse** — `NEXTAUTH_SECRET`, `SALT`
4. **outline** — `SECRET_KEY`, `UTILS_SECRET`
5. **dify** — `SECRET_KEY`, `REDIS_PASSWORD`
6. **paperless-ngx** — `PAPERLESS_SECRET_KEY`
