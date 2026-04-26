#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

IMAGE_TAG="ato-wrong-runtime-e2e:local"
NO_CACHE=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-cache) NO_CACHE="--no-cache"; shift ;;
        --image) IMAGE_TAG="$2"; shift 2 ;;
        *) echo "Unknown arg: $1"; exit 1 ;;
    esac
done

command -v docker >/dev/null || { echo "ERROR: docker not found"; exit 1; }
docker info >/dev/null 2>&1 || { echo "ERROR: Docker daemon is not running."; exit 1; }

echo "▶ Building wrong-runtime Docker E2E image..."
docker build \
    $NO_CACHE \
    --platform linux/amd64 \
    -f "$SCRIPT_DIR/Dockerfile.wrong-runtime-e2e" \
    -t "$IMAGE_TAG" \
    "$REPO_ROOT"

echo "▶ Running wrong-runtime Docker E2E..."
docker run \
    --rm \
    --network bridge \
    --platform linux/amd64 \
    -e ATO_NO_INTERACTIVE=1 \
    "$IMAGE_TAG"
