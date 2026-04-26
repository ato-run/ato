#!/usr/bin/env bash
# Day 6.5 — Seed the local registry with fixture capsules for desktop E2E demo.
#
# Usage:
#   ./scripts/seed_local_registry.sh [DATA_DIR]
#
# DATA_DIR defaults to ~/.ato/registry (the desktop's default).
# The script inserts two capsules into registry.sqlite3:
#   1. ato/openclaw-local-llm  — zero-config (no required_env)
#   2. ato/byok-ai-chat        — requires OPENAI_API_KEY (triggers ConfigModal)
#
# Prerequisites:
#   - The registry must NOT be running (sqlite WAL lock).
#     Stop it first, run this script, then restart.
#   - sqlite3 CLI must be on PATH.

set -euo pipefail

DATA_DIR="${1:-${HOME}/.ato/registry}"
DB="${DATA_DIR}/registry.sqlite3"
NOW="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

if [[ ! -f "${DB}" ]]; then
  echo "[ERROR] Database not found: ${DB}" >&2
  echo "        Start the registry once first to initialise the schema:" >&2
  echo "        ato registry serve --data-dir '${DATA_DIR}'" >&2
  exit 1
fi

if ! command -v sqlite3 >/dev/null 2>&1; then
  echo "[ERROR] sqlite3 is required but not found on PATH" >&2
  exit 1
fi

echo "[INFO] Seeding ${DB}"
echo "[INFO] Timestamp: ${NOW}"

# Read the real capsule.toml files as manifest_toml blobs.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

OPENCLAW_TOML="${REPO_ROOT}/samples/openclaw-local-llm/capsule.toml"
BYOK_TOML="${REPO_ROOT}/samples/byok-ai-chat/capsule.toml"

if [[ ! -f "${OPENCLAW_TOML}" ]]; then
  echo "[ERROR] Missing ${OPENCLAW_TOML}" >&2
  exit 1
fi
if [[ ! -f "${BYOK_TOML}" ]]; then
  echo "[ERROR] Missing ${BYOK_TOML}" >&2
  exit 1
fi

OPENCLAW_TOML_CONTENT="$(cat "${OPENCLAW_TOML}")"
BYOK_TOML_CONTENT="$(cat "${BYOK_TOML}")"

# Synthetic hashes — search endpoint never validates these; they only
# matter for download/detail views which are out of demo scope.
HASH_OPENCLAW="blake3:0000000000000000000000000000000000000000000000000000000000000001"
HASH_BYOK="blake3:0000000000000000000000000000000000000000000000000000000000000002"
MERKLE_OPENCLAW="blake3:f000000000000000000000000000000000000000000000000000000000000001"
MERKLE_BYOK="blake3:f000000000000000000000000000000000000000000000000000000000000002"

sqlite3 "${DB}" <<SQL
-- Ensure FK checks are on for this session.
PRAGMA foreign_keys = ON;

-- 1) Synthetic manifests (required by registry_releases FK).
INSERT OR IGNORE INTO manifests(manifest_hash, manifest_toml, merkle_root, signer_set, created_at)
VALUES
  ('${HASH_OPENCLAW}', '$(echo "${OPENCLAW_TOML_CONTENT}" | sed "s/'/''/g")', '${MERKLE_OPENCLAW}', 'did:key:local:ato', '${NOW}'),
  ('${HASH_BYOK}',     '$(echo "${BYOK_TOML_CONTENT}"    | sed "s/'/''/g")', '${MERKLE_BYOK}',     'did:key:local:ato', '${NOW}');

-- 2) Registry packages.
INSERT OR IGNORE INTO registry_packages(scoped_id, publisher, slug, name, description, latest_version, created_at, updated_at)
VALUES
  ('ato/openclaw-local-llm', 'ato', 'openclaw-local-llm',
   'OpenClaw + Ollama (Local LLM)',
   'OpenClaw AI agent powered by local Ollama - no API keys required',
   '0.1.0', '${NOW}', '${NOW}'),
  ('ato/byok-ai-chat', 'ato', 'byok-ai-chat',
   'BYOK AI Chat',
   'Minimal AI chat app with Bring Your Own Key support',
   '0.1.1', '${NOW}', '${NOW}');

-- 3) Registry releases (one per capsule).
INSERT OR IGNORE INTO registry_releases(
  scoped_id, version, manifest_hash, file_name,
  sha256, blake3, size_bytes, signature_status, created_at
)
VALUES
  ('ato/openclaw-local-llm', '0.1.0', '${HASH_OPENCLAW}',
   'openclaw-local-llm-0.1.0.capsule',
   '0000000000000000000000000000000000000000000000000000000000000001',
   '0000000000000000000000000000000000000000000000000000000000000001',
   1024, 'verified', '${NOW}'),
  ('ato/byok-ai-chat', '0.1.1', '${HASH_BYOK}',
   'byok-ai-chat-0.1.1.capsule',
   '0000000000000000000000000000000000000000000000000000000000000002',
   '0000000000000000000000000000000000000000000000000000000000000002',
   2048, 'verified', '${NOW}');

SQL

echo "[OK] Inserted 2 capsules into registry."
echo ""
echo "Verify with:"
echo "  ato registry serve --data-dir '${DATA_DIR}' &"
echo "  curl -s 'http://127.0.0.1:8787/v1/capsules?q=' | python3 -m json.tool"
