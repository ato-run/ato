#!/usr/bin/env bash
#
# Operator-side steps for updating a Formation worker.
#
# The worker never installs its own binary. This script prepares and records
# the operator's side of the update, and prints the actions that must stay in
# human hands (the systemd drop-in, the restart, the undrain). It does not
# build, download, sign or deploy anything.
#
# Default is DRY-RUN: every command, journal change and unit edit is printed
# and nothing is touched. Pass --execute to perform the recorded steps.
#
# Runbook: docs/ops/formation-worker-update-lifecycle.md
# Wire contract: documented in docs/ops/formation-worker-update-lifecycle.md
#
# Subcommands:
#   status         - show the journal, the installed binary and the service state
#   update         - verify the candidate, request the update, wait for
#                    quiescence, record `updating`, install the binary
#   rollback       - restore the previous binary and clear the journal
#   clear-journal  - remove a finished (quiescent) journal after undrain
#
# Exit codes of `ato-formation-worker serve` (for the operator reading logs):
#   0   normal
#   74  rollback required / journal quarantined - do not restart in a loop
#   75  drained / quiescent - start again after the swap or maintenance
#   1   unexpected error
#
# Dependencies: bash, sha256sum (or shasum), python3 (atomic journal writes),
# curl (admin request). Deliberately no jq requirement.

set -euo pipefail

PROG=$(basename "$0")
EXECUTE=0
DRY_RUN=1
POLL_SECONDS=5
TIMEOUT_SECONDS=900
ALLOW_DIRTY=0
FORCE_TERMINAL=0

CANDIDATE=""
TARGET_VERSION=""
TARGET_DIGEST=""
API_BASE=""
RUNNER_ID=""
WORK_ROOT=""
JOURNAL=""
INSTALL_TO=""
SERVICE=""
PREVIOUS_BINARY=""

usage() {
    cat <<EOF
Usage: $PROG <status|update|rollback|clear-journal> [options]

Common:
  --work-root DIR        worker work root (journal lives here)
  --install-to PATH      installed binary path (default /usr/local/bin/ato-formation-worker)
  --service NAME         systemd service name (default ato-formation-worker.service)
  --execute              perform the steps (default: dry-run)
  --help

update:
  --candidate PATH       candidate binary already staged on this host (required)
  --target-version X.Y.Z required
  --target-digest HEX    required; 64 hex, sha256 of the candidate
  --api-base URL         control plane base URL (required with --execute)
  --runner-id ID         runner device id (required with --execute)
  --allow-dirty-build    allow a commit=dirty/unknown candidate (release blocker)
  --timeout-seconds N    how long to wait for the worker to become quiescent

rollback:
  --previous-binary PATH override the recorded previous binary

clear-journal:
  --force-terminal       allow removing rolled_back / quarantined journals

Environment:
  ATO_ADMIN_TOKEN        admin token for the update request (x-ato-admin-token)
  ATO_ADMIN_BEARER       admin bearer token, used instead when set

The script never runs the service, edits systemd units, or undrains. Those
steps are printed as OPERATOR ACTION lines and must be done deliberately.
EOF
}

info() {
    printf '%s\n' "$*"
}

step() {
    printf '\n== %s ==\n' "$*"
}

die() {
    printf '%s: error: %s\n' "$PROG" "$*" >&2
    exit 1
}

# Print (dry-run) or run a command, never enabling shell tracing (a token could
# appear in an argument).
run() {
    if [ "$EXECUTE" -eq 1 ]; then
        "$@"
    else
        printf 'dry-run:'
        printf ' %q' "$@"
        printf '\n'
    fi
}

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        die "sha256sum (or shasum) is required"
    fi
}

journal_python() {
    python3 - "$@" <<'PY'
import datetime
import json
import os
import re
import sys
import tempfile

def die(message):
    print(f"journal: error: {message}", file=sys.stderr)
    sys.exit(1)

def read(path):
    try:
        with open(path, "r", encoding="utf-8") as handle:
            return json.load(handle)
    except FileNotFoundError:
        return None
    except (OSError, ValueError) as error:
        die(f"{path} is not a readable update journal: {error}")

def normalize_digest(value):
    value = value.strip()
    if value.startswith("sha256:"):
        value = value[len("sha256:"):]
    if not re.fullmatch(r"[0-9a-fA-F]{64}", value):
        die(f"not a sha256 digest: {value!r}")
    return value.lower()

def now():
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

def atomic_write(path, record):
    directory = os.path.dirname(os.path.abspath(path))
    os.makedirs(directory, exist_ok=True)
    handle = tempfile.NamedTemporaryFile(
        mode="w", encoding="utf-8", dir=directory,
        prefix=".worker-update-", suffix=".tmp", delete=False,
    )
    temporary = handle.name
    try:
        json.dump(record, handle, indent=2)
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())
        handle.close()
        os.replace(temporary, path)
    finally:
        if not handle.closed:
            handle.close()
        if os.path.exists(temporary):
            os.unlink(temporary)
    # Best effort: the rename is what publishes the record.
    try:
        directory_fd = os.open(directory, os.O_RDONLY)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    except OSError:
        pass

def main(argv):
    if len(argv) < 2:
        die("expected a command and a journal path")
    command, path = argv[0], argv[1]
    options = {}
    index = 2
    while index < len(argv):
        key = argv[index]
        if not key.startswith("--") or index + 1 >= len(argv):
            die(f"bad journal argument: {key}")
        options[key[2:].replace("-", "_")] = argv[index + 1]
        index += 2

    record = read(path)

    if command == "phase":
        print("missing" if record is None else record.get("phase"))
        return
    if command == "show":
        print(json.dumps(record, indent=2) if record is not None else "missing")
        return
    if command == "previous-binary":
        print(record.get("previous_binary") or "" if record else "")
        return
    if command == "set-updating":
        if record is None:
            die(f"{path} is missing; the worker has no drain record to advance")
        expected = options.get("expect", "quiescent")
        if record.get("phase") != expected:
            die(f"journal phase is {record.get('phase')!r}, expected {expected!r}")
        target_version = options.get("target_version", "").strip()
        if not target_version:
            die("--target-version is required")
        record["phase"] = "updating"
        record["target_version"] = target_version
        record["target_digest"] = normalize_digest(options["target_digest"])
        if options.get("previous_binary"):
            record["previous_binary"] = options["previous_binary"]
        record["detail"] = "operator recorded the binary swap"
        record["updated_at"] = now()
        atomic_write(path, record)
        print(f"journal: {expected} -> updating")
        return
    die(f"unknown journal command: {command}")

main(sys.argv[1:])
PY
}

journal_phase() {
    journal_python phase "$JOURNAL"
}

journal_show() {
    journal_python show "$JOURNAL"
}

journal_previous_binary() {
    journal_python previous-binary "$JOURNAL"
}

path_defaults() {
    if [ -z "$WORK_ROOT" ]; then
        die "--work-root is required"
    fi
    if [ -z "$JOURNAL" ]; then
        JOURNAL="$WORK_ROOT/worker-update.json"
    fi
    if [ -z "$INSTALL_TO" ]; then
        INSTALL_TO="/usr/local/bin/ato-formation-worker"
    fi
    if [ -z "$SERVICE" ]; then
        SERVICE="ato-formation-worker.service"
    fi
    command -v python3 >/dev/null 2>&1 || die "python3 is required for journal reads/writes"
}

service_state() {
    if command -v systemctl >/dev/null 2>&1; then
        systemctl is-active "$SERVICE" 2>/dev/null || true
    else
        printf 'unknown (systemctl not available)'
    fi
}

describe_phase() {
    case "$1" in
        active) info "phase active: the worker serves and claims work" ;;
        draining) info "phase draining: claims stopped; an in-flight job may still run" ;;
        quiescent) info "phase quiescent: no job in flight; the worker exits 75 when drained" ;;
        updating) info "phase updating: the binary swap is in progress (or was interrupted)" ;;
        verifying) info "phase verifying: the new binary is being matched against the target" ;;
        rolled_back) info "phase rolled_back: exit 74; reconcile the binary and clear the journal" ;;
        quarantined) info "phase quarantined: exit 74; inspect worker-update.json.corrupt" ;;
        missing) info "phase missing: no update in progress (contract: absent journal reads as active)" ;;
        *) info "phase $1: unknown to this script" ;;
    esac
}

cmd_status() {
    path_defaults
    step "Journal"
    local phase
    phase=$(journal_phase)
    journal_show
    describe_phase "$phase"

    step "Installed binary"
    if [ -f "$INSTALL_TO" ]; then
        printf '%s\n' "$INSTALL_TO"
        printf 'sha256: %s\n' "$(sha256_file "$INSTALL_TO")"
        if [ -x "$INSTALL_TO" ]; then
            "$(absolute_hint "$INSTALL_TO")" --version || true
        fi
    else
        info "missing: $INSTALL_TO"
    fi
    if [ -f "$INSTALL_TO.prev" ]; then
        printf 'previous: %s (sha256 %s)\n' "$INSTALL_TO.prev" "$(sha256_file "$INSTALL_TO.prev")"
    fi

    step "Service"
    printf '%s: %s\n' "$SERVICE" "$(service_state)"
}

# Parse one option shared by every subcommand. On success CONSUMED is the
# number of arguments used; callers shift by it. Returns 1 when the option is
# not common, so the subcommand can parse it.
CONSUMED=1
parse_common_one() {
    CONSUMED=1
    case "$1" in
        --execute) EXECUTE=1; DRY_RUN=0 ;;
        --work-root) WORK_ROOT="${2:?--work-root needs a value}"; CONSUMED=2 ;;
        --journal) JOURNAL="${2:?--journal needs a value}"; CONSUMED=2 ;;
        --install-to) INSTALL_TO="${2:?--install-to needs a value}"; CONSUMED=2 ;;
        --service) SERVICE="${2:?--service needs a value}"; CONSUMED=2 ;;
        --help|-h) usage; exit 0 ;;
        *) return 1 ;;
    esac
    return 0
}

# A bare file name is not on PATH; make the path explicit before running it.
absolute_hint() {
    case "$1" in
        */*) printf '%s\n' "$1" ;;
        *) printf './%s\n' "$1" ;;
    esac
}

verify_candidate() {
    [ -n "$CANDIDATE" ] || die "--candidate is required"
    CANDIDATE=$(absolute_hint "$CANDIDATE")
    [ -f "$CANDIDATE" ] || die "candidate not found: $CANDIDATE"
    [ -x "$CANDIDATE" ] || die "candidate is not executable: $CANDIDATE"
    [ -n "$TARGET_VERSION" ] || die "--target-version is required"
    [ -n "$TARGET_DIGEST" ] || die "--target-digest is required"
    TARGET_DIGEST=$(printf '%s' "$TARGET_DIGEST" | tr 'A-F' 'a-f')
    case "$TARGET_DIGEST" in
        sha256:*) TARGET_DIGEST=${TARGET_DIGEST#sha256:} ;;
    esac
    if ! printf '%s' "$TARGET_DIGEST" | grep -Eq '^[0-9a-f]{64}$'; then
        die "--target-digest must be 64 hex characters: $TARGET_DIGEST"
    fi

    local actual
    actual=$(sha256_file "$CANDIDATE")
    [ "$actual" = "$TARGET_DIGEST" ] || die "candidate sha256 is $actual, not the target $TARGET_DIGEST"

    # `--version` is the binary's claim; the digest above is what the bytes
    # actually are. Both are recorded together in the deployment record.
    local line version commit dirty
    line=$("$CANDIDATE" --version | head -n 1)
    version=$(printf '%s\n' "$line" | awk '{print $2}')
    commit=$(printf '%s\n' "$line" | sed -n 's/.*commit=\([^ ]*\).*/\1/p')
    dirty=$(printf '%s\n' "$line" | awk '{for (i = 1; i <= NF; i++) if ($i == "clean" || $i == "dirty" || $i == "unknown") { print $i; exit }}')
    [ "$version" = "$TARGET_VERSION" ] || die "candidate reports version $version, not the target $TARGET_VERSION"
    if [ "$dirty" != "clean" ] && [ "$ALLOW_DIRTY" -eq 0 ]; then
        die "candidate is a '$dirty' build (commit=$commit); a dirty/unknown build is a release blocker"
    fi
    info "candidate: $CANDIDATE"
    info "  sha256:  $actual"
    info "  version: $version commit=$commit $dirty"
}

request_update() {
    [ -n "$API_BASE" ] || die "--api-base is required"
    [ -n "$RUNNER_ID" ] || die "--runner-id is required"
    local url body
    url="${API_BASE%/}/v1/admin/runners/${RUNNER_ID}/update"
    body=$(printf '{"target_version":"%s","target_digest":"%s"}' "$TARGET_VERSION" "$TARGET_DIGEST")
    info "POST $url"
    info "  body: $body"
    if [ "$EXECUTE" -eq 1 ]; then
        local auth
        if [ -n "${ATO_ADMIN_BEARER:-}" ]; then
            auth=(-H "Authorization: Bearer ${ATO_ADMIN_BEARER}")
        else
            [ -n "${ATO_ADMIN_TOKEN:-}" ] || die "ATO_ADMIN_TOKEN (or ATO_ADMIN_BEARER) is required with --execute"
            auth=(-H "x-ato-admin-token: ${ATO_ADMIN_TOKEN}")
        fi
        curl --fail-with-body -sS -X POST "$url" \
            -H "content-type: application/json" "${auth[@]}" --data "$body"
        printf '\n'
    else
        printf 'dry-run: curl --fail-with-body -sS -X POST %q -H "content-type: application/json" -H "<admin auth redacted>" --data %q\n' "$url" "$body"
    fi
}

wait_for_phase() {
    local wanted=$1 phase elapsed=0
    if [ "$DRY_RUN" -eq 1 ]; then
        info "dry-run: would wait until the journal phase is '$wanted' (timeout ${TIMEOUT_SECONDS}s)"
        return 0
    fi
    while :; do
        phase=$(journal_phase)
        if [ "$phase" = "$wanted" ]; then
            info "journal phase: $wanted"
            return 0
        fi
        if [ "$elapsed" -ge "$TIMEOUT_SECONDS" ]; then
            die "timed out after ${TIMEOUT_SECONDS}s waiting for '$wanted' (journal: $phase)"
        fi
        sleep "$POLL_SECONDS"
        elapsed=$((elapsed + POLL_SECONDS))
    done
}

cmd_update() {
    path_defaults
    verify_candidate

    step "Request the update (control plane drains the worker)"
    request_update

    step "Wait for quiescence"
    if [ "$DRY_RUN" -eq 0 ]; then
        local before
        before=$(journal_phase)
        [ "$before" != "missing" ] || die "no journal at $JOURNAL; the worker has not drained (was the update request accepted?)"
        info "journal phase before waiting: $before"
    fi
    info "the worker finishes the in-flight job, records quiescent and exits 75"
    wait_for_phase quiescent
    if [ "$DRY_RUN" -eq 1 ]; then
        info "dry-run: would then check phase $(journal_phase) and record updating"
        return 0
    fi

    step "Record 'updating' in the journal"
    local previous=""
    if [ -f "$INSTALL_TO" ]; then
        previous="$INSTALL_TO.prev"
    fi
    journal_python set-updating "$JOURNAL" \
        --expect quiescent \
        --target-version "$TARGET_VERSION" \
        --target-digest "$TARGET_DIGEST" \
        --previous-binary "${previous:-}"

    step "Install the candidate"
    if [ -f "$INSTALL_TO" ]; then
        info "keeping the current binary at $INSTALL_TO.prev"
        if [ "$EXECUTE" -eq 0 ]; then
            printf 'dry-run: cp -p %q %q\n' "$INSTALL_TO" "$INSTALL_TO.prev"
        else
            cp -p "$INSTALL_TO" "$INSTALL_TO.prev"
        fi
    fi
    run install -m 0755 "$CANDIDATE" "$INSTALL_TO.new"
    if [ "$EXECUTE" -eq 1 ]; then
        local installed
        installed=$(sha256_file "$INSTALL_TO.new")
        [ "$installed" = "$TARGET_DIGEST" ] || die "the installed bytes are $installed, not the target digest"
        mv "$INSTALL_TO.new" "$INSTALL_TO"
        info "installed: $INSTALL_TO (sha256 $installed)"
    fi

    step "OPERATOR ACTION: point the unit at the new binary"
    cat <<EOF
1. Edit the systemd drop-in so ExecStart names $INSTALL_TO with the
   expectation flags, for example:

     [Service]
     ExecStart=
     ExecStart=$INSTALL_TO serve \\
       --api-base <url> --work-root $WORK_ROOT --worker-id <id> \\
       --expect-version $TARGET_VERSION --expect-digest $TARGET_DIGEST

2. sudo systemctl daemon-reload
3. sudo systemctl restart $SERVICE
4. Re-run: $PROG status --work-root $WORK_ROOT --install-to $INSTALL_TO --service $SERVICE
   The journal must be 'active' and the sha256 must equal $TARGET_DIGEST.
5. POST /v1/admin/runners/${RUNNER_ID:-<runner_id>}/undrain once verified, then
   smoke one Formation job.
EOF
}

cmd_rollback() {
    path_defaults
    local phase
    phase=$(journal_phase)
    case "$phase" in
        updating|verifying|rolled_back|quarantined) ;;
        *) die "refusing to roll back from journal phase '$phase'; expected updating/verifying/rolled_back/quarantined" ;;
    esac
    if [ "$phase" = "quarantined" ] && [ ! -f "$JOURNAL.corrupt" ]; then
        info "warning: no $JOURNAL.corrupt found; the quarantined record is not the original bytes"
    fi

    local previous="$PREVIOUS_BINARY"
    if [ -z "$previous" ]; then
        previous=$(journal_previous_binary)
    fi
    if [ -z "$previous" ]; then
        previous="$INSTALL_TO.prev"
    fi
    previous=$(absolute_hint "$previous")
    [ -f "$previous" ] || die "previous binary not found: $previous (pass --previous-binary)"
    [ -x "$previous" ] || die "previous binary is not executable: $previous"

    step "Restore the previous binary"
    info "previous: $previous"
    info "  sha256:  $(sha256_file "$previous")"
    "$previous" --version || true
    if [ -f "$INSTALL_TO" ]; then
        local failed="$INSTALL_TO.failed-$(sha256_file "$INSTALL_TO" | cut -c1-12)"
        info "keeping the failed binary at $failed"
        if [ "$EXECUTE" -eq 0 ]; then
            printf 'dry-run: cp -p %q %q\n' "$INSTALL_TO" "$failed"
        else
            cp -p "$INSTALL_TO" "$failed"
        fi
    fi
    run install -m 0755 "$previous" "$INSTALL_TO.new"
    if [ "$EXECUTE" -eq 1 ]; then
        mv "$INSTALL_TO.new" "$INSTALL_TO"
    fi

    step "Clear the terminal journal"
    info "rolled_back/quarantined are terminal by design; an absent journal reads as active"
    run rm -f "$JOURNAL"

    step "OPERATOR ACTION: finish the rollback"
    cat <<EOF
1. Remove or reset the --expect-version/--expect-digest flags in the drop-in to
   match the restored binary, then sudo systemctl daemon-reload.
2. sudo systemctl restart $SERVICE
3. Re-run: $PROG status --work-root $WORK_ROOT --install-to $INSTALL_TO --service $SERVICE
4. Confirm the heartbeat reports the restored agent_version, then undrain and
   smoke one Formation job. Record the failed target and the mismatch.
EOF
}

cmd_clear_journal() {
    path_defaults
    local phase
    phase=$(journal_phase)
    case "$phase" in
        missing) info "nothing to clear: $JOURNAL is absent" ; return 0 ;;
        quiescent) ;;
        rolled_back|quarantined)
            [ "$FORCE_TERMINAL" -eq 1 ] || die "journal is '$phase' (terminal); pass --force-terminal after verifying the binary"
            ;;
        *) die "refusing to clear journal phase '$phase'; only quiescent (or terminal with --force-terminal) may be cleared" ;;
    esac
    info "removing $JOURNAL (phase $phase); the next start reads this as active"
    run rm -f "$JOURNAL"
}

main() {
    [ $# -gt 0 ] || { usage; exit 1; }
    local command=$1 mutating=0
    shift

    case "$command" in
        status) ;;
        update|rollback|clear-journal) mutating=1 ;;
        --help|-h) usage; exit 0 ;;
        *) usage; exit 1 ;;
    esac

    while [ $# -gt 0 ]; do
        if parse_common_one "$@"; then
            shift "$CONSUMED"
            continue
        fi
        case "$command:$1" in
            update:--candidate) CANDIDATE="${2:?--candidate needs a value}"; shift 2 ;;
            update:--target-version) TARGET_VERSION="${2:?--target-version needs a value}"; shift 2 ;;
            update:--target-digest) TARGET_DIGEST="${2:?--target-digest needs a value}"; shift 2 ;;
            update:--api-base) API_BASE="${2:?--api-base needs a value}"; shift 2 ;;
            update:--runner-id) RUNNER_ID="${2:?--runner-id needs a value}"; shift 2 ;;
            update:--timeout-seconds) TIMEOUT_SECONDS="${2:?--timeout-seconds needs a value}"; shift 2 ;;
            update:--allow-dirty-build) ALLOW_DIRTY=1; shift ;;
            rollback:--previous-binary) PREVIOUS_BINARY="${2:?--previous-binary needs a value}"; shift 2 ;;
            clear-journal:--force-terminal) FORCE_TERMINAL=1; shift ;;
            *) die "unknown option for $command: $1" ;;
        esac
    done

    case "$command" in
        status) cmd_status ;;
        update) cmd_update ;;
        rollback) cmd_rollback ;;
        clear-journal) cmd_clear_journal ;;
    esac

    if [ "$mutating" -eq 1 ] && [ "$DRY_RUN" -eq 1 ]; then
        printf '\n(dry-run: nothing was changed; re-run with --execute to perform the recorded steps)\n'
    fi
}

main "$@"
