#!/bin/bash
# GPU availability check script for Capsuled Engine
# Usage: check-gpu.sh [--json]

set -e

OUTPUT_JSON=false
if [[ "$1" == "--json" ]]; then
    OUTPUT_JSON=true
fi

# Check if nvidia-smi is available
if ! command -v nvidia-smi &> /dev/null; then
    if $OUTPUT_JSON; then
        echo '{"available": false, "error": "nvidia-smi not found"}'
    else
        echo "Error: nvidia-smi not found. NVIDIA driver may not be installed."
    fi
    exit 1
fi

# Get GPU info
GPU_INFO=$(nvidia-smi --query-gpu=name,memory.total,memory.free,memory.used,utilization.gpu,temperature.gpu --format=csv,noheader,nounits 2>/dev/null)

if [[ -z "$GPU_INFO" ]]; then
    if $OUTPUT_JSON; then
        echo '{"available": false, "error": "No GPU detected"}'
    else
        echo "Error: No GPU detected."
    fi
    exit 1
fi

if $OUTPUT_JSON; then
    # Parse and output as JSON
    echo "{"
    echo '  "available": true,'
    echo '  "gpus": ['
    
    first=true
    while IFS=',' read -r name total free used util temp; do
        if $first; then
            first=false
        else
            echo ","
        fi
        # Trim whitespace
        name=$(echo "$name" | xargs)
        total=$(echo "$total" | xargs)
        free=$(echo "$free" | xargs)
        used=$(echo "$used" | xargs)
        util=$(echo "$util" | xargs)
        temp=$(echo "$temp" | xargs)
        
        echo "    {"
        echo "      \"name\": \"$name\","
        echo "      \"memory_total_mb\": $total,"
        echo "      \"memory_free_mb\": $free,"
        echo "      \"memory_used_mb\": $used,"
        echo "      \"utilization_percent\": $util,"
        echo "      \"temperature_celsius\": $temp"
        echo -n "    }"
    done <<< "$GPU_INFO"
    
    echo ""
    echo "  ]"
    echo "}"
else
    echo "=== GPU Status ==="
    echo ""
    nvidia-smi --query-gpu=index,name,memory.total,memory.free,utilization.gpu --format=csv
    echo ""
    echo "=== Driver Info ==="
    nvidia-smi --query-gpu=driver_version,cuda_version --format=csv,noheader | head -1
fi
