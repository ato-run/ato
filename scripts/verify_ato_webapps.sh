#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DEFAULT_CSV="$REPO_ROOT/.tmp/ato_cli_webapps.csv"
DEFAULT_OUT_DIR="$REPO_ROOT/.tmp/ato-cli-verification/$(date +%Y%m%d-%H%M%S)"

CSV_FILE="$DEFAULT_CSV"
OUT_DIR="$DEFAULT_OUT_DIR"
LIMIT=100
OFFSET=0
TIMEOUT_SECONDS=90
SLEEP_SECONDS=18
CREATE_ISSUE=1
ISSUE_REPO=""
ATO_BIN="${ATO_BIN:-}"

RESULTS_FILE=""
SUMMARY_FILE=""
TARGETS_FILE=""
LOG_DIR=""
RUNS_DIR=""
RUN_STARTED_AT=""
RUN_FINISHED_AT=""
RUN_EXIT_CODE=0
RUN_TIMED_OUT=0

usage() {
    cat <<EOF
Usage: $(basename "$0") [options]

Options:
  --csv PATH          CSV file to read (default: $DEFAULT_CSV)
  --out-dir PATH      Output directory for results.md, summary.md, and logs
  --limit N           Number of repositories to process (default: 100, 0 = all)
  --offset N          Number of CSV rows to skip before processing (default: 0)
  --timeout N         Seconds to allow each ato run before interrupting (default: 90)
  --sleep N           Seconds to wait between repositories (default: 180)
  --ato-bin PATH      Path to ato binary (default: repo build, then PATH lookup)
  --issue-repo REPO   Override gh issue target repo (owner/name)
  --no-issue          Skip gh issue create; still writes summary.md
  --help              Show this help text
EOF
}

fail() {
    echo "Error: $*" >&2
    exit 1
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

resolve_ato_bin() {
    if [ -n "$ATO_BIN" ]; then
        [ -x "$ATO_BIN" ] || fail "ATO_BIN is not executable: $ATO_BIN"
        return
    fi

    if [ -x "$REPO_ROOT/apps/ato-cli/target/release/ato" ]; then
        ATO_BIN="$REPO_ROOT/apps/ato-cli/target/release/ato"
        return
    fi

    if command -v ato >/dev/null 2>&1; then
        ATO_BIN="$(command -v ato)"
        return
    fi

    fail "could not find ato binary; build apps/ato-cli or pass --ato-bin"
}

parse_args() {
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --csv)
                CSV_FILE="$2"
                shift 2
                ;;
            --out-dir)
                OUT_DIR="$2"
                shift 2
                ;;
            --limit)
                LIMIT="$2"
                shift 2
                ;;
            --offset)
                OFFSET="$2"
                shift 2
                ;;
            --timeout)
                TIMEOUT_SECONDS="$2"
                shift 2
                ;;
            --sleep)
                SLEEP_SECONDS="$2"
                shift 2
                ;;
            --ato-bin)
                ATO_BIN="$2"
                shift 2
                ;;
            --issue-repo)
                ISSUE_REPO="$2"
                shift 2
                ;;
            --no-issue)
                CREATE_ISSUE=0
                shift
                ;;
            --help)
                usage
                exit 0
                ;;
            *)
                fail "unknown argument: $1"
                ;;
        esac
    done
}

validate_args() {
    [ -f "$CSV_FILE" ] || fail "CSV file not found: $CSV_FILE"
    [[ "$LIMIT" =~ ^[0-9]+$ ]] || fail "--limit must be a non-negative integer"
    [[ "$OFFSET" =~ ^[0-9]+$ ]] || fail "--offset must be a non-negative integer"
    [[ "$TIMEOUT_SECONDS" =~ ^[0-9]+$ ]] || fail "--timeout must be a non-negative integer"
    [[ "$SLEEP_SECONDS" =~ ^[0-9]+$ ]] || fail "--sleep must be a non-negative integer"
}

prepare_paths() {
    mkdir -p "$OUT_DIR"
    LOG_DIR="$OUT_DIR/logs"
    RUNS_DIR="$OUT_DIR/isolation"
    mkdir -p "$LOG_DIR" "$RUNS_DIR"
    RESULTS_FILE="$OUT_DIR/results.md"
    SUMMARY_FILE="$OUT_DIR/summary.md"
    TARGETS_FILE="$OUT_DIR/targets.tsv"
}

initialize_results_file() {
    RUN_STARTED_AT="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

    cat > "$RESULTS_FILE" <<EOF
# Ato Verification Results

- Started: $RUN_STARTED_AT
- CSV: $CSV_FILE
- Offset: $OFFSET
- Limit: $LIMIT
- Timeout Seconds: $TIMEOUT_SECONDS
- Sleep Seconds: $SLEEP_SECONDS
- Ato Binary: $ATO_BIN

## Results
EOF
}

build_targets_file() {
    ruby -rcsv -e '
        csv_path, offset_str, limit_str = ARGV
        offset = offset_str.to_i
        limit = limit_str.to_i
        emitted = 0

        CSV.foreach(csv_path, headers: true).with_index do |row, index|
          next if index < offset
          break if limit > 0 && emitted >= limit

          repo = row["repo"].to_s.strip
          url = row["url"].to_s.strip
          next if repo.empty? || url.empty?

          puts [index + 1, repo, url].join("\t")
          emitted += 1
        end
    ' "$CSV_FILE" "$OFFSET" "$LIMIT" > "$TARGETS_FILE"

    if [ ! -s "$TARGETS_FILE" ]; then
        fail "no repositories selected from CSV"
    fi
}

sanitize_name() {
    printf '%s' "$1" | tr '/:' '__' | tr -cd 'A-Za-z0-9._-'
}

has_start_signal() {
    local log_file="$1"
    grep -Eiq 'listening (on|at)|ready on|running (at|on)|local:[[:space:]]*http|http://(127\.0\.0\.1|localhost|0\.0\.0\.0)|application startup complete|uvicorn running on|serving on|started server|accepting connections|server started' "$log_file"
}

has_fatal_signal() {
    local log_file="$1"
    grep -Eiq 'traceback|panic|fatal|segmentation fault|permission denied|address already in use|eaddrinuse|no such file|cannot find|not found|exception|error:|failed' "$log_file"
}

extract_log_snippet() {
    ruby -e '
        path = ARGV[0]
        text = File.read(path, encoding: "UTF-8")
        lines = text.lines.map { |line| line.strip.gsub(/\s+/, " ") }.reject(&:empty?)
        interesting = lines.reverse.find { |line| line =~ /(traceback|panic|fatal|permission denied|address already in use|eaddrinuse|no such file|cannot find|not found|exception|error:|failed|listening|ready on|running at|running on|local:|http:\/\/|startup complete|serving on|server started)/i }
        interesting ||= lines.last
        interesting ||= "no output captured"
        print interesting[0, 280]
    ' "$1"
}

run_with_timeout() {
    local url="$1"
    local log_file="$2"
    local env_root="$3"
    local original_home="$HOME"
    local cmd_pid watchdog_pid

    mkdir -p "$env_root/home" "$env_root/cache" "$env_root/config" "$env_root/data" "$env_root/tmp" "$env_root/state"

    (
        export HOME="$env_root/home"
        export XDG_CACHE_HOME="$env_root/cache"
        export XDG_CONFIG_HOME="$env_root/config"
        export XDG_DATA_HOME="$env_root/data"
        export XDG_STATE_HOME="$env_root/state"
        export TMPDIR="$env_root/tmp"
        export PIP_CACHE_DIR="$env_root/cache/pip"
        export UV_CACHE_DIR="$env_root/cache/uv"
        export POETRY_CACHE_DIR="$env_root/cache/pypoetry"
        export npm_config_cache="$env_root/cache/npm"
        export YARN_CACHE_FOLDER="$env_root/cache/yarn"
        export BUN_INSTALL_CACHE_DIR="$env_root/cache/bun"
        export PNPM_HOME="$env_root/pnpm-home"
        export COMPOSER_HOME="$env_root/composer"
        export GEM_HOME="$env_root/gem-home"
        export GEM_SPEC_CACHE="$env_root/cache/gem"
        export BUNDLE_PATH="$env_root/bundle"
        export GOCACHE="$env_root/cache/go-build"
        export RUSTUP_HOME="${RUSTUP_HOME:-$original_home/.rustup}"
        export CARGO_HOME="${CARGO_HOME:-$original_home/.cargo}"
        "$ATO_BIN" run "$url" --yes --compatibility-fallback host
    ) > "$log_file" 2>&1 &
    cmd_pid=$!

    (
        sleep "$TIMEOUT_SECONDS"
        if kill -0 "$cmd_pid" 2>/dev/null; then
            printf '__ATO_TIMEOUT__\n' >> "$log_file"
            kill -INT "$cmd_pid" 2>/dev/null || true
            sleep 5
            if kill -0 "$cmd_pid" 2>/dev/null; then
                kill -TERM "$cmd_pid" 2>/dev/null || true
                sleep 5
            fi
            if kill -0 "$cmd_pid" 2>/dev/null; then
                kill -KILL "$cmd_pid" 2>/dev/null || true
            fi
        fi
    ) &
    watchdog_pid=$!

    set +e
    wait "$cmd_pid"
    RUN_EXIT_CODE=$?
    set -e

    kill "$watchdog_pid" 2>/dev/null || true
    wait "$watchdog_pid" 2>/dev/null || true

    if grep -q '^__ATO_TIMEOUT__$' "$log_file"; then
        RUN_TIMED_OUT=1
    else
        RUN_TIMED_OUT=0
    fi
}

append_result() {
    local ordinal="$1"
    local total="$2"
    local csv_index="$3"
    local repo="$4"
    local url="$5"
    local status="$6"
    local detail="$7"
    local log_file="$8"

    cat >> "$RESULTS_FILE" <<EOF

### [$ordinal/$total] $repo
- CSV Row: $csv_index
- URL: $url
- Status: $status
- Detail: $detail
- Log File: $log_file
EOF
}

build_summary() {
    RUN_FINISHED_AT="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
    printf '\n- Finished: %s\n' "$RUN_FINISHED_AT" >> "$RESULTS_FILE"

    ruby -e '
        path = ARGV[0]
        text = File.read(path, encoding: "UTF-8")
        started = text[/^- Started: (.+)$/, 1] || "unknown"
        finished = text[/^- Finished: (.+)$/, 1] || "unknown"
        statuses = text.scan(/^- Status: (Success|Failure)$/).flatten

        failure_urls = []
        current_url = nil

        text.each_line do |line|
          current_url = Regexp.last_match(1) if line =~ /^- URL: (.+)$/
          if line =~ /^- Status: Failure$/ && current_url
            failure_urls << current_url
          end
        end

        puts "# Ato Verification Summary"
        puts
        puts "- Period: #{started} to #{finished}"
        puts "- Total Repositories: #{statuses.length}"
        puts "- Success: #{statuses.count("Success")}"
        puts "- Failure: #{statuses.count("Failure")}"
        puts
        puts "## Failed Repository URLs"

        if failure_urls.empty?
          puts "- None"
        else
          failure_urls.uniq.each { |url| puts "- #{url}" }
        end
    ' "$RESULTS_FILE" > "$SUMMARY_FILE"
}

create_issue() {
    local title="Ato Verification Summary: $(date +%Y-%m-%d)"

    if [ "$CREATE_ISSUE" -eq 0 ]; then
        echo "Skipping gh issue create because --no-issue was specified."
        return
    fi

    require_command gh

    if [ -n "$ISSUE_REPO" ]; then
        (cd "$REPO_ROOT" && gh issue create -R "$ISSUE_REPO" --title "$title" --body-file "$SUMMARY_FILE")
    else
        (cd "$REPO_ROOT" && gh issue create --title "$title" --body-file "$SUMMARY_FILE")
    fi
}

main() {
    local total current csv_index repo url safe_name log_file env_root snippet status detail

    parse_args "$@"
    require_command ruby
    validate_args
    resolve_ato_bin
    prepare_paths
    initialize_results_file
    build_targets_file

    total="$(wc -l < "$TARGETS_FILE" | tr -d '[:space:]')"
    current=0

    while IFS=$'\t' read -r csv_index repo url; do
        current=$((current + 1))
        safe_name="$(sanitize_name "$repo")"
        log_file="$LOG_DIR/$(printf '%03d' "$current")-${safe_name}.log"
        env_root="$RUNS_DIR/$(printf '%03d' "$current")-${safe_name}"

        echo "[$current/$total] $repo"
        run_with_timeout "$url" "$log_file" "$env_root"
        snippet="$(extract_log_snippet "$log_file")"

        if [ "$RUN_EXIT_CODE" -eq 0 ]; then
            status="Success"
            detail="exit=0; $snippet"
        elif [ "$RUN_TIMED_OUT" -eq 1 ] && has_start_signal "$log_file" && ! has_fatal_signal "$log_file"; then
            status="Success"
            detail="timed out after startup signal; $snippet"
        else
            status="Failure"
            if [ "$RUN_TIMED_OUT" -eq 1 ]; then
                detail="timed out without a clean startup signal; $snippet"
            else
                detail="exit=$RUN_EXIT_CODE; $snippet"
            fi
        fi

        append_result "$current" "$total" "$csv_index" "$repo" "$url" "$status" "$detail" "$log_file"

        if [ "$current" -lt "$total" ] && [ "$SLEEP_SECONDS" -gt 0 ]; then
            sleep "$SLEEP_SECONDS"
        fi
    done < "$TARGETS_FILE"

    build_summary
    create_issue

    echo "Results written to $RESULTS_FILE"
    echo "Summary written to $SUMMARY_FILE"
}

main "$@"
