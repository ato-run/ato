#!/bin/bash
# Mock Runtime for Testing
# This simulates a simple runtime that can be started and stopped

set -e

# Parse arguments
PID_FILE=""
while [[ $# -gt 0 ]]; do
    case $1 in
        --pid-file)
            PID_FILE="$2"
            shift 2
            ;;
        *)
            echo "Unknown argument: $1" >&2
            shift
            ;;
    esac
done

# Write PID to file if specified
if [ -n "$PID_FILE" ]; then
    echo $$ > "$PID_FILE"
fi

# Log startup
echo "[mock_runtime] Started with PID $$" >&2
echo "[mock_runtime] PORT=${PORT:-unset}" >&2

# Simple HTTP server simulation - just keep running
echo "[mock_runtime] Running... (press Ctrl+C to stop)" >&2

# Trap termination signals
trap 'echo "[mock_runtime] Received termination signal, exiting..." >&2; exit 0' SIGTERM SIGINT

# Keep running (simulating a long-running service)
while true; do
    sleep 1
done
