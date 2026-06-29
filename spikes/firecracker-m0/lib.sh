#!/usr/bin/env bash
# Shared helpers for the M0 Firecracker spike (throwaway). Drives Firecracker via its
# REST API over a unix socket. Requires /dev/kvm. KVM-free parts are in build-guest.sh.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "${here}/versions.env"

mkdir -p "${SPIKE_WORK}"

fc_api() { # METHOD PATH [JSON]
  local method="$1" path="$2" body="${3:-}"
  if [ -n "$body" ]; then
    curl -fsS --unix-socket "${API_SOCK}" -X "$method" "http://localhost${path}" \
      -H 'Content-Type: application/json' -d "$body"
  else
    curl -fsS --unix-socket "${API_SOCK}" -X "$method" "http://localhost${path}"
  fi
}

require_kvm() {
  if [ ! -r /dev/kvm ] || [ ! -w /dev/kvm ]; then
    echo "FATAL: /dev/kvm not usable. Pick a KVM host (BM.Standard.A1.160 or x86 nested-virt)." >&2
    exit 2
  fi
}

start_firecracker() { # SOCK_PATH
  export API_SOCK="$1"
  rm -f "${API_SOCK}"
  ./firecracker --api-sock "${API_SOCK}" --id "spike-$$" >"${SPIKE_WORK}/fc.log" 2>&1 &
  FC_PID=$!
  for _ in $(seq 1 50); do [ -S "${API_SOCK}" ] && return 0; sleep 0.05; done
  echo "FATAL: firecracker API socket never appeared" >&2; exit 1
}

configure_vm() {
  fc_api PUT /boot-source "{\"kernel_image_path\":\"${GUEST_KERNEL}\",\"boot_args\":\"console=ttyS0 reboot=k panic=1 pci=off\"}"
  fc_api PUT /drives/rootfs "{\"drive_id\":\"rootfs\",\"path_on_host\":\"${ROOTFS}\",\"is_root_device\":true,\"is_read_only\":false}"
  fc_api PUT /network-interfaces/eth0 '{"iface_id":"eth0","host_dev_name":"fc-tap0"}'
  fc_api PUT /vsock '{"vsock_id":"vsock0","guest_cid":3,"uds_path":"'"${SPIKE_WORK}"'/vsock.sock"}'
  if [ "${CPU_TEMPLATE}" != "none" ]; then
    fc_api PUT /machine-config "{\"vcpu_count\":1,\"mem_size_mib\":256,\"cpu_template\":\"${CPU_TEMPLATE}\"}"
  else
    fc_api PUT /machine-config '{"vcpu_count":1,"mem_size_mib":256}'
  fi
}

boot_vm() { fc_api PUT /actions '{"action_type":"InstanceStart"}'; }
pause_vm() { fc_api PATCH /vm '{"state":"Paused"}'; }
resume_vm() { fc_api PATCH /vm '{"state":"Resumed"}'; }

wait_healthy() { # returns ms-to-first-200 on stdout
  local start now
  start=$(date +%s%3N)
  for _ in $(seq 1 600); do
    if curl -fsS "http://${GUEST_IP:-172.16.0.2}:${GUEST_PORT}${HEALTH_PATH}" >/dev/null 2>&1; then
      now=$(date +%s%3N); echo $((now - start)); return 0
    fi
    sleep 0.05
  done
  echo "FATAL: guest never became healthy" >&2; return 1
}

create_snapshot() { # MEM_FILE STATE_FILE
  pause_vm
  fc_api PUT /snapshot/create "{\"snapshot_type\":\"Full\",\"snapshot_path\":\"$2\",\"mem_file_path\":\"$1\"}"
}

load_snapshot() { # MEM_FILE STATE_FILE (File backend)
  fc_api PUT /snapshot/load "{\"snapshot_path\":\"$2\",\"mem_backend\":{\"backend_type\":\"File\",\"backend_path\":\"$1\"},\"resume_vm\":true}"
}

cleanup() { [ -n "${FC_PID:-}" ] && kill "${FC_PID}" 2>/dev/null || true; }
trap cleanup EXIT
