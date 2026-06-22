#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# docker_no_runtime_e2e.sh
# Outer orchestrator: builds the Docker test image, then runs the E2E suite.
#
# Usage:
#   ./tests/docker_no_runtime_e2e.sh [--no-cache] [--image <tag>]
#
# The image is a multi-stage build:
#   - builder: compiles ato from source (rust:1.80-slim-bookworm)
#   - tester:  Debian slim with NO node / npm / python3
#
# Requires: docker (tested with 24+)
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

IMAGE_TAG="ato-no-runtime-e2e:local"
NO_CACHE=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-cache) NO_CACHE="--no-cache"; shift ;;
        --image)    IMAGE_TAG="$2"; shift 2 ;;
        *)          echo "Unknown arg: $1"; exit 1 ;;
    esac
done

# ─── Pre-flight ───────────────────────────────────────────────────────────────
if ! command -v docker &>/dev/null; then
    echo "ERROR: docker not found. Install Docker Desktop or Docker Engine first."
    exit 1
fi

if ! docker info &>/dev/null; then
    echo "ERROR: Docker daemon is not running."
    exit 1
fi

echo "═══════════════════════════════════════════════════"
echo " ato Docker E2E — no-node / no-python environment"
echo "═══════════════════════════════════════════════════"
echo "Image:       $IMAGE_TAG"
echo "Build root:  $REPO_ROOT"
echo ""

# ─── Build ────────────────────────────────────────────────────────────────────
echo "▶ Building Docker image (this takes ~10 min on first run; cached thereafter)…"
docker build \
    $NO_CACHE \
    --platform linux/amd64 \
    -f "$SCRIPT_DIR/Dockerfile.no-runtime-e2e" \
    -t "$IMAGE_TAG" \
    "$REPO_ROOT" \
    2>&1 | sed 's/^/  [build] /'

echo ""
echo "▶ Image built. Running E2E tests inside container…"
echo ""

# ─── Run ──────────────────────────────────────────────────────────────────────
# --rm:       remove container after exit
# --network:  'bridge' allows ato to download managed Node from nodejs.org
#             and npm packages from registry.npmjs.org
docker run \
    --rm \
    --network bridge \
    --platform linux/amd64 \
    -e ATO_NO_INTERACTIVE=1 \
    "$IMAGE_TAG"

EXIT_CODE=$?

echo ""
if [ "$EXIT_CODE" -eq 0 ]; then
    echo "✓ All Docker E2E tests passed."
else
    echo "✗ Docker E2E tests FAILED (exit $EXIT_CODE)."
fi

exit "$EXIT_CODE"
