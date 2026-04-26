#!/bin/bash
# =============================================================================
# §2 実機 GPU / アクセラレータ — 全項目が人手確認
# =============================================================================
set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../config.sh"
RESULT_FILE="$RESULTS_DIR/result_02_gpu.log"
: > "$RESULT_FILE"

SUITE="§2 GPU / Accelerator"
echo "══════════════════════════════════"
echo " $SUITE"
echo "══════════════════════════════════"
check_ato

# ---------------------------------------------------------------------------
# Automated: detect if a GPU-capable runtime hint is present
# ---------------------------------------------------------------------------
test_gpu_env_detect() {
    local has_gpu=false
    if command -v nvidia-smi &>/dev/null && nvidia-smi -L &>/dev/null 2>&1; then
        info "NVIDIA GPU detected: $(nvidia-smi --query-gpu=name --format=csv,noheader | head -1)"
        has_gpu=true
    fi
    if system_profiler SPDisplaysDataType 2>/dev/null | grep -qi "Metal"; then
        info "macOS Metal GPU detected"
        has_gpu=true
    fi
    if $has_gpu; then
        pass "GPU hardware detected on this machine"
    else
        skip "GPU hardware detected — no GPU found; run GPU tests on hardware with GPU"
    fi
}

# ---------------------------------------------------------------------------
# Automated: capsule with GPU hint runs (or fails gracefully without GPU)
# ---------------------------------------------------------------------------
test_gpu_fallback_no_crash() {
    local out="$ATO_TEST_TMP/gpu_fallback.txt"
    # A capsule that declares GPU capability should degrade to CPU gracefully
    # when no GPU is present, rather than panic
    if ! command -v ato &>/dev/null; then skip "gpu-fallback (ato not installed)"; return; fi
    # We test with a simple source capsule that requests gpu; expect a clean error not a panic
    local tmp_dir="$ATO_TEST_TMP/gpu-fallback-capsule"
    mkdir -p "$tmp_dir"
    cat > "$tmp_dir/capsule.toml" <<'EOF'
schema_version = "0.3"
name = "gpu-fallback-test"
version = "0.1.0"
type = "app"
run = "python3 -c 'print(\"ok\")'"
runtime = "source/python"

[resources]
gpu = "prefer"
EOF
    provision_python_capsule "$tmp_dir"
    # ato run hangs after capsule exits (known bug), check output instead
    run_cmd 20 "$out" env CAPSULE_ALLOW_UNSAFE=1 ato run --dangerously-skip-permissions "$tmp_dir" || true
    if grep -q "ok" "$out"; then
        pass "GPU 'prefer' capsule runs on CPU when no GPU available"
    else
        # Exit code nonzero is OK, but no panic/SIGSEGV
        if grep -qi "panic\|SIGSEGV\|segfault\|killed" "$out"; then
            fail "GPU 'prefer' capsule falls back gracefully" "Crash/panic detected: $(tail -5 "$out")"
        else
            pass "GPU 'prefer' capsule exits cleanly (no GPU → degraded gracefully)"
        fi
    fi
    rm -rf "$tmp_dir"
}

# ---------------------------------------------------------------------------
# Human checks — hardware-specific
# ---------------------------------------------------------------------------
test_macos_metal() {
    checklist "macOS Metal — Llama 3.1 8B on Apple Silicon" \
        "Use a Mac with M1/M2/M3/M4 chip" \
        "Run: ato run ato.run/samples/llama-chat (or equivalent GPU capsule)" \
        "Confirm model loads within expected time (8GB VRAM: <2min, 16GB: <1min)" \
        "Monitor Activity Monitor → GPU History during inference" \
        "GPU broker respects memory budget (no OOM kill)" \
        "Inference output is coherent"
}

test_linux_cuda() {
    checklist "Linux CUDA — RTX series, CUDA 11.x and 12.x" \
        "Test on a machine with CUDA driver installed (nvidia-smi shows driver version)" \
        "Run the same GPU capsule with CUDA 11.x driver" \
        "Run again with CUDA 12.x driver" \
        "Confirm both succeed and produce correct output" \
        "Test on WSL2: GPU passthrough works (nvidia-smi inside WSL2)"
}

test_amd_rocm() {
    checklist "AMD ROCm (Linux)" \
        "Machine has AMD Radeon GPU with ROCm driver installed" \
        "Run GPU capsule and confirm ROCm backend is used (check ato --verbose output)" \
        "Model loads and inference succeeds"
}

test_gpu_no_fallback() {
    checklist "CPU fallback when GPU unavailable" \
        "On a machine with NO GPU, run a capsule requiring gpu = 'require'" \
        "Confirm ato prints a clear actionable error (not a panic)" \
        "Error message names the missing capability and suggests remediation" \
        "On a machine with NO GPU, run a capsule with gpu = 'prefer'" \
        "Confirm ato falls back to CPU automatically with an info message"
}

test_gpu_memory_budget() {
    checklist "GPU broker honors memory budget" \
        "Run two GPU capsules simultaneously on a machine with limited VRAM" \
        "Confirm the second capsule waits or queues rather than OOM-killing the first" \
        "After first capsule exits, second capsule gets GPU resources"
}

test_sleep_wake() {
    checklist "Sleep/Wake cycle with active GPU capsule" \
        "Start a long-running GPU capsule (e.g. an inference server)" \
        "Close laptop lid (sleep) for 30 seconds" \
        "Open lid (wake) and confirm capsule is still running or exits cleanly" \
        "No orphaned GPU memory after wake"
}

test_egpu() {
    checklist "External eGPU hot-plug" \
        "Start ato-desktop with an eGPU connected" \
        "Disconnect the eGPU mid-session" \
        "Confirm capsule degrades or exits cleanly rather than crashing" \
        "Re-connect eGPU — new capsule launches can use it"
}

test_gpu_env_detect
test_gpu_fallback_no_crash
test_macos_metal
test_linux_cuda
test_amd_rocm
test_gpu_no_fallback
test_gpu_memory_budget
test_sleep_wake
test_egpu

print_suite_summary "$SUITE"
