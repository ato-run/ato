#!/usr/bin/env python3
"""Sequential, reversible 5b-b mutation probes; never runs implicitly.

Run only after other Cargo jobs and source edits stop. --scope core (default)
mutates generation_context.rs only; --scope all explicitly also mutates the
provider v2 validator. Tests themselves are never mutated. No Git operations,
remote services, provider credentials or dependency downloads are used.

Each named test must pass before mutation, fail at runtime (not compilation)
under its single mutant, and pass again after byte-for-byte restoration. SIGINT,
SIGTERM, timeout and exceptions restore the original. SIGKILL/power failure cannot
run finally: durable .original backups plus report.json provide manual recovery.
"""
from __future__ import annotations

import argparse
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
CORE = "lib/formation/src/generation_context.rs"
PROVIDER = "apps/formation-worker/src/generation_provider.rs"


@dataclass(frozen=True)
class Mutant:
    name: str
    file: str
    old: str
    new: str
    package: str
    suite: str
    test: str


MUTANTS = [
    Mutant("core_entry_count", CORE,
           "self.entrypoints.len() > MAX_ENTRIES",
           "self.entrypoints.len() > MAX_ENTRIES + 1",
           "ato-formation", "generation_context",
           "mutation_entry_count_bound_rejects_seventeen_otherwise_valid_entries"),
    Mutant("core_wire_bytes", CORE,
           "if bytes.len() > MAX_CONTEXT_BYTES {",
           "if false && bytes.len() > MAX_CONTEXT_BYTES {",
           "ato-formation", "generation_context",
           "mutation_wire_byte_bound_rejects_valid_json_with_excess_padding"),
    Mutant("core_source_id_canary", CORE,
           "id: id.into(),",
           "id: String::from_utf8_lossy(bytes).into_owned(),",
           "ato-formation", "generation_context",
           "mutation_source_identifier_canary_never_becomes_the_output_id"),
    Mutant("core_string_shield", CORE,
           "                i = end;",
           "                out.extend(lex(&source[i + 1..end - 1])?);\n                i = end;",
           "ato-formation", "generation_context",
           "strings_comments_triples_and_fstrings_never_supply_markers"),
    Mutant("v2_exact_ids", PROVIDER,
           "if ids != context_ids {", "if false && ids != context_ids {",
           "ato-formation-worker", "generation_provider_v1",
           "v2_rejects_missing_mismatched_invalid_and_oversized_context_before_http"),
    Mutant("v2_context_validation", PROVIDER,
           'context.validate().map_err(|_| "invalid")?;',
           'let _unchecked_context = context;',
           "ato-formation-worker", "generation_provider_v1",
           "v2_rejects_missing_mismatched_invalid_and_oversized_context_before_http"),
    Mutant("v2_point_schema", PROVIDER,
           "if point.schema != GENERATION_POINT_SCHEMA_V2 {",
           "if false && point.schema != GENERATION_POINT_SCHEMA_V2 {",
           "ato-formation-worker", "generation_provider_v1",
           "v2_rejects_missing_mismatched_invalid_and_oversized_context_before_http"),
]


def command(m: Mutant) -> list[str]:
    return ["cargo", "test", "--locked", "--offline", "-p", m.package,
            "--test", m.suite, m.test, "--", "--exact", "--test-threads=1"]


def stop_child(proc: subprocess.Popen) -> None:
    if proc.poll() is None:
        os.killpg(proc.pid, signal.SIGTERM)
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid, signal.SIGKILL)
            proc.wait()


def run_test(m: Mutant, log: Path, timeout: int) -> dict:
    env = dict(os.environ, CARGO_TERM_COLOR="never", RUST_BACKTRACE="0")
    with log.open("wb") as output:
        proc = subprocess.Popen(command(m), cwd=ROOT, env=env,
                                stdout=output, stderr=subprocess.STDOUT,
                                start_new_session=True)
        try:
            rc = proc.wait(timeout=timeout)
        finally:
            stop_child(proc)
    text = log.read_text(errors="replace")
    return {"returncode": rc, "log": str(log), "command": command(m),
            "passed": rc == 0 and re.search(rf"test {re.escape(m.test)} \.\.\. ok", text) is not None,
            "named_failure": rc != 0 and re.search(rf"test {re.escape(m.test)} \.\.\. FAILED", text) is not None
                and "test result: FAILED" in text}


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def ensure_no_cargo_jobs() -> None:
    # Conservative across repositories: a source restore must not race a compiler.
    names = subprocess.check_output(["ps", "-axo", "comm="], text=True)
    active = [name.strip() for name in names.splitlines()
              if Path(name.strip()).name in {"cargo", "rustc", "rustdoc"}]
    if active:
        raise RuntimeError("Cargo/Rust jobs are active; wait before mutation probes")


def interrupted(signum, _frame):
    raise InterruptedError(f"interrupted by signal {signum}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--list", action="store_true")
    mode.add_argument("--run", action="store_true")
    parser.add_argument("--scope", choices=["core", "provider", "all"], default="core")
    parser.add_argument("--timeout-seconds", type=int, default=300)
    args = parser.parse_args()
    if args.timeout_seconds < 1:
        parser.error("timeout must be positive")
    selected = [m for m in MUTANTS if args.scope == "all"
                or (args.scope == "core") == (m.file == CORE)]
    if args.list:
        for m in selected:
            print(json.dumps({"mutant": m.name, "source": m.file,
                              "named_test": m.test, "command": command(m)}))
        return 0
    ensure_no_cargo_jobs()
    home = ROOT / ".tmp/generation-context/mutations"
    home.mkdir(parents=True, exist_ok=True)
    lock = home / "active.lock"
    # Leave a clear recoverable lock on SIGKILL instead of silently stacking probes.
    fd = os.open(lock, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    os.write(fd, str(os.getpid()).encode())
    os.close(fd)
    out = home / f"run-{time.time_ns()}"
    out.mkdir()
    report = {"root": str(ROOT), "scope": args.scope, "results": []}

    def save_report():
        serialized = json.dumps(report, indent=2) + "\n"
        (out / "report.json").write_text(serialized)
        (home.parent / "mutations.json").write_text(serialized)

    old_handlers = {s: signal.signal(s, interrupted) for s in (signal.SIGINT, signal.SIGTERM)}
    try:
        originals = {m.file: (ROOT / m.file).read_bytes() for m in selected}
        # Preflight every anchor before modifying any source, preserving current
        # dirty/untracked bytes, NOT whatever happens to be in Git HEAD.
        for m in selected:
            if originals[m.file].count(m.old.encode()) != 1:
                raise RuntimeError(f"{m.name}: expected exactly one mutation anchor")
        for relative, original in originals.items():
            (out / (relative.replace("/", "__") + ".original")).write_bytes(original)
        save_report()
        for m in selected:
            path = ROOT / m.file
            original = originals[m.file]
            if path.read_bytes() != original:
                raise RuntimeError(f"{m.name}: concurrent edit detected before mutation")
            record = {"mutant": m.name, "source": m.file, "named_test": m.test,
                      "original_sha256": sha(original), "status": "baseline"}
            report["results"].append(record)
            record["baseline"] = run_test(m, out / f"{m.name}.baseline.log", args.timeout_seconds)
            if not record["baseline"]["passed"]:
                raise RuntimeError(f"{m.name}: baseline did not run and pass the named test")
            ensure_no_cargo_jobs()
            if path.read_bytes() != original:
                raise RuntimeError(f"{m.name}: concurrent edit detected during baseline")
            mutated = original.replace(m.old.encode(), m.new.encode(), 1)
            record["status"] = "mutating"
            save_report()
            try:
                path.write_bytes(mutated)
                record["mutant_run"] = run_test(m, out / f"{m.name}.mutant.log", args.timeout_seconds)
                record["status"] = "killed" if record["mutant_run"]["named_failure"] else "not_killed"
            finally:
                # No branch changes or git checkout: restore exactly these bytes.
                # Preserve unexpected concurrent edits separately rather than lose them.
                current = path.read_bytes()
                if current not in (original, mutated):
                    recovery = out / f"{m.name}.concurrent-edit"
                    recovery.write_bytes(current)
                    record["concurrent_edit_backup"] = str(recovery)
                path.write_bytes(original)
                record["restored_sha256"] = sha(path.read_bytes())
                record["restored"] = record["restored_sha256"] == record["original_sha256"]
                save_report()
            record["restored_test"] = run_test(m, out / f"{m.name}.restored.log", args.timeout_seconds)
            save_report()
            print(json.dumps(record), flush=True)
            if (record["status"] != "killed" or not record["restored"]
                    or not record["restored_test"]["passed"] or "concurrent_edit_backup" in record):
                raise RuntimeError(f"{m.name}: failed mutation/restore verification; inspect report")
        report["status"] = "all_killed_and_restored"
        return 0
    except BaseException as error:
        report["status"] = "incomplete"
        report["error"] = str(error)
        raise
    finally:
        save_report()
        for sig, handler in old_handlers.items():
            signal.signal(sig, handler)
        lock.unlink(missing_ok=True)
        print(f"Mutation report: {out / 'report.json'}", flush=True)


if __name__ == "__main__":
    sys.exit(main())
