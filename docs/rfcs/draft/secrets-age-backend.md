# RFC: Secret Storage Architecture — age Backend

**Status:** Draft  
**App:** `ato-cli`  
**Scope:** `apps/ato-cli/src/application/secrets/`

---

## Problem

The previous `ato secrets` implementation used the OS keychain (`keyring` crate) as the primary
persistent store. This caused:

- Slow or hanging prompts in headless / CI environments
- Linux instability (no unified keyring daemon)
- Namespace and ACL metadata stored separately in `metadata.json`, creating split-brain risk
- No portable encrypted backup / export path

---

## Design

### Backend priority chain

Resolution order (highest to lowest):

| Priority | Backend         | Source                        | Writable | Notes |
|----------|----------------|-------------------------------|----------|-------|
| 1        | `EnvBackend`   | `ATO_SECRET_<KEY>` env vars   | ✗        | CI override; keys normalized UPPER_SNAKE |
| 2        | `MemoryBackend`| In-process HashMap + TTL      | ✓        | Ephemeral; default TTL configurable |
| 3        | `AgeFileBackend` | `~/.ato/secrets/<ns>.age`  | ✓        | Primary persistent store |
| 4        | Legacy keychain | OS keychain / `.env` file    | ✓        | Fallback when no age identity exists |

The order can be customized via `~/.ato/config.toml`:

```toml
[secrets]
backends = ["env", "memory", "age"]  # omit "keychain" to disable legacy fallback
```

### File layout

```
~/.ato/
├── keys/
│   ├── identity.key        # X25519 private key (plain text or passphrase-encrypted age armor) chmod 600
│   └── identity.pub        # bech32 recipient public key
├── secrets/
│   ├── default.age         # namespace = default
│   ├── publish.age         # namespace = publish
│   └── capsule_<name>.age  # namespace = capsule:<name>  (`:` → `_` in filename)
└── run/
    └── session-{pid}.key   # unlocked identity for session; chmod 600
```

Each `.age` file contains an age-encrypted JSON envelope:

```json
{
  "schema_version": "0.1",
  "namespace": "default",
  "created_at": "2025-01-01T00:00:00Z",
  "updated_at": "2025-01-01T00:00:00Z",
  "entries": {
    "OPENAI_API_KEY": {
      "value": "sk-...",
      "created_at": "2025-01-01T00:00:00Z",
      "updated_at": "2025-01-01T00:00:00Z",
      "description": null,
      "allow": null,
      "deny": null
    }
  }
}
```

### Crypto

- Algorithm: X25519 key agreement + ChaCha20-Poly1305 AEAD (standard age format)
- Crate: [`age = "0.10"`](https://crates.io/crates/age)
- Identity generation: `age::x25519::Identity::generate()`
- Passphrase option: identity key itself is stored as an age-armored file encrypted with a passphrase (passphrase-encrypted identity)
- Atomic write: encrypt to `<ns>.age.tmp.<pid>` → `fs::rename()`
- Advisory lock: `fs2::FileExt::try_lock_exclusive()` with 5-second retry

### Session unlock (`ato session`)

Prompting once per shell session:

```
ato session start [--ttl 8h]
```

This:
1. Loads and decrypts `identity.key` (prompting for passphrase if needed)
2. Writes plain `AGE-SECRET-KEY-1...` to `~/.ato/run/session-{pid}.key` (chmod 600)
3. Writes expiry metadata to `~/.ato/run/session-{pid}.key.meta`
4. Prints `export ATO_SESSION_KEY_FILE=~/.ato/run/session-{pid}.key`

Child processes check `ATO_SESSION_KEY_FILE` to skip re-prompting. Stale files (dead PID or expired TTL) are cleaned up on the next `session start`.

```
ato session end     # delete session key file
ato session status  # show active session info
```

### ACL (allow/deny lists)

Stored inside the `.age` JSON per entry. Evaluated at `ato secrets load --for <capsule_id>`:

```toml
# capsule.toml example (consumer side)
[secrets]
required = ["OPENAI_API_KEY"]
```

```
ato secrets allow OPENAI_API_KEY --capsule my-capsule
ato secrets deny  OPENAI_API_KEY --capsule untrusted-capsule
```

### CLI commands

| Command | Description |
|---------|-------------|
| `ato secrets init [--passphrase] [--no-passphrase]` | Generate `identity.key` + `identity.pub` |
| `ato secrets set KEY [--namespace NS] [--description D]` | Set secret (reads value from stdin or prompt) |
| `ato secrets get KEY [--namespace NS]` | Print secret value |
| `ato secrets list [--namespace NS] [--json]` | List entries |
| `ato secrets delete KEY [--namespace NS]` | Delete entry |
| `ato secrets import --file F [--namespace NS]` | Import from `.env` file |
| `ato secrets allow KEY --capsule ID` | Add capsule to allow-list |
| `ato secrets deny  KEY --capsule ID` | Add capsule to deny-list |
| `ato secrets rotate-identity [--passphrase]` | Re-encrypt all `.age` files with a new key |
| `ato session start [--ttl DURATION]` | Unlock identity for session |
| `ato session end` | Revoke session |
| `ato session status` | Show session info |

---

## Migration from legacy keychain

When no `~/.ato/keys/identity.key` exists, `SecretStore::open()` silently falls back to the
legacy keychain path (same behavior as v0.4 and earlier). Users can migrate at their own pace:

```
ato secrets init          # create age identity
ato secrets import --from-env ~/.ato/secrets.env  # import existing .env backup
```

The legacy keychain backend remains in the priority chain as position 4 until explicitly removed
via `~/.ato/config.toml`.

---

## Security properties

- **At-rest encryption:** all secrets encrypted with X25519 + ChaCha20-Poly1305
- **Key isolation:** `identity.key` chmod 600; session files chmod 600
- **CI safety:** `EnvBackend` at priority 1 ensures CI never touches disk secrets
- **Capability gating:** ACL stored inside encrypted file; evaluated before returning value
- **No plaintext logging:** `age::secrecy::Secret<String>` wraps private key material

---

## Rejected alternatives

| Alternative | Reason rejected |
|-------------|----------------|
| SOPS native integration | Go-binary dependency; no stable Rust crate; `import/export` escape hatch sufficient |
| HashiCorp Vault | External service dependency; contradicts "Safe by default, self-contained" philosophy |
| Keep keychain as primary | Unstable in Linux/CI; no namespace support; no portable backup |
| `rage` crate instead of `age` | `age` 0.10 is the canonical upstream Rust impl; `rage` is a wrapper |
