#!/usr/bin/env bash
set -euo pipefail

FILES="${FILES:-10000}"
SIZE_BYTES="${SIZE_BYTES:-1024}"
RESULT_DIR="${RESULT_DIR:-/tmp/tar-pack-benchmark}"
mkdir -p "${RESULT_DIR}"

JSON_PATH="${RESULT_DIR}/result-files-${FILES}.json"
TIME_PATH="${RESULT_DIR}/time-files-${FILES}.txt"
SUMMARY_PATH="${RESULT_DIR}/summary-files-${FILES}.txt"

echo "tar_pack_benchmark: files=${FILES}, size_bytes=${SIZE_BYTES}"

time_cmd=()
if command -v gtime >/dev/null 2>&1; then
  time_cmd=(gtime -v)
elif /usr/bin/time -v true >/dev/null 2>&1; then
  time_cmd=(/usr/bin/time -v)
elif /usr/bin/time -l true >/dev/null 2>&1; then
  time_cmd=(/usr/bin/time -l)
fi

if [[ "${#time_cmd[@]}" -gt 0 ]]; then
  "${time_cmd[@]}" cargo run -p capsule-core --bin tar_pack_bench -- \
    --files "${FILES}" \
    --size-bytes "${SIZE_BYTES}" \
    >"${JSON_PATH}" \
    2>"${TIME_PATH}"
else
  cargo run -p capsule-core --bin tar_pack_bench -- \
    --files "${FILES}" \
    --size-bytes "${SIZE_BYTES}" \
    >"${JSON_PATH}"
  : >"${TIME_PATH}"
fi

pack_elapsed_ms="$(sed -nE 's/.*"pack_elapsed_ms":([0-9]+).*/\1/p' "${JSON_PATH}" | tail -n1)"
total_elapsed_ms="$(sed -nE 's/.*"total_elapsed_ms":([0-9]+).*/\1/p' "${JSON_PATH}" | tail -n1)"

peak_rss_kb="unknown"
wall_clock="unknown"
if [[ -s "${TIME_PATH}" ]]; then
  peak_rss_kb="$(awk '
    /Maximum resident set size/ {
      split($0, parts, ":");
      value=parts[2];
      gsub(/^[ \t]+/, "", value);
      print value;
    }
    /maximum resident set size/ {
      print $1;
    }
  ' "${TIME_PATH}" | tail -n1)"
  wall_clock="$(awk '
    /Elapsed \(wall clock\) time/ {
      split($0, parts, ":");
      value=parts[2];
      gsub(/^[ \t]+/, "", value);
      print value;
    }
    / real$/ {
      print $1 "s";
    }
  ' "${TIME_PATH}" | tail -n1)"
fi

cat >"${SUMMARY_PATH}" <<EOF
files=${FILES}
size_bytes=${SIZE_BYTES}
pack_elapsed_ms=${pack_elapsed_ms}
total_elapsed_ms=${total_elapsed_ms}
peak_rss_kb=${peak_rss_kb}
wall_clock=${wall_clock}
result_json=${JSON_PATH}
time_report=${TIME_PATH}
EOF

cat "${SUMMARY_PATH}"
cat "${JSON_PATH}"

if [[ -n "${MAX_PACK_ELAPSED_MS:-}" && -n "${pack_elapsed_ms}" ]]; then
  if (( pack_elapsed_ms > MAX_PACK_ELAPSED_MS )); then
    echo "pack_elapsed_ms ${pack_elapsed_ms} exceeded MAX_PACK_ELAPSED_MS ${MAX_PACK_ELAPSED_MS}" >&2
    exit 1
  fi
fi

if [[ -n "${MAX_PEAK_RSS_KB:-}" && "${peak_rss_kb}" != "unknown" ]]; then
  if (( peak_rss_kb > MAX_PEAK_RSS_KB )); then
    echo "peak_rss_kb ${peak_rss_kb} exceeded MAX_PEAK_RSS_KB ${MAX_PEAK_RSS_KB}" >&2
    exit 1
  fi
fi
