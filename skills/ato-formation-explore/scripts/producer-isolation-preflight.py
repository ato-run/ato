#!/usr/bin/env python3
"""Prepare a public Skill package and measure a macOS OS fixture, never an agent."""

import argparse
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import platform
import re
import secrets
import socket
import stat
import subprocess
import sys


SKILL = Path(__file__).resolve().parents[1]
MCP_TOOLS = ["status", "next", "submit", "cancel"]
CANARIES = ["source", "owner-credential", "owner-connection", "private-grant", "runtime-ticket",
            "state-db", "old-measurements", "credential-store-file"]
MACH_AUTH = ["com.apple.secd", "com.apple.SecurityServer"]
OS_CHECKS = ["public_skill_read", "public_skill_write_denied",
             "private_eight_classes_read_denied", "private_eight_classes_write_denied",
             "scratch_write", "symlink_escape_denied", "fork_inherits_denial",
             "source_direct_launch_denied", "shell_exec_denied", "security_exec_denied",
             "curl_exec_denied", "docker_unix_socket_denied", "relay_tcp_allowed",
             "provider_fixture_tcp_allowed", "relay_wrong_port_denied", "relay_wrong_address_denied",
             "provider_wrong_port_denied", "provider_wrong_address_denied", "owner_unix_socket_denied",
             "ipv6_denied", "udp_denied", "extra_descriptors_absent"]
MACHO = {b"\xcf\xfa\xed\xfe", b"\xfe\xed\xfa\xcf", b"\xce\xfa\xed\xfe",
         b"\xfe\xed\xfa\xce", b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca",
         b"\xca\xfe\xba\xbf", b"\xbf\xba\xfe\xca"}


class Rejected(Exception):
    """Only bounded, constant rejection codes reach public output."""


def reject(code):
    raise Rejected(code)


def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as stream:
        while data := stream.read(1024 * 1024):
            result.update(data)
    return result.hexdigest()


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode()


def safe_path(path):
    path = Path(path).resolve(strict=True)
    if any(ord(c) < 32 or ord(c) == 127 for c in str(path)):
        reject("path_control_character")
    return path


def quoted(path):
    return json.dumps(str(safe_path(path)))


def binary(path):
    path = safe_path(path)
    info = path.stat()
    if not stat.S_ISREG(info.st_mode) or not info.st_mode & 0o111:
        reject("native_binary_required")
    if info.st_mode & (stat.S_ISUID | stat.S_ISGID) or info.st_size > 256 * 1024 * 1024:
        reject("native_binary_bounds")
    with path.open("rb") as stream:
        if stream.read(4) not in MACHO:
            reject("native_macho_required")
    return {"path": str(path), "sha256": digest(path)}


def version_tuple(value):
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", value):
        reject("exact_version_required")
    return tuple(map(int, value.split(".")))


def tcp_endpoint(value):
    """Numeric IPv4 only: no DNS, wildcard, port range, URL, or SBPL injection."""
    if not isinstance(value, str) or len(value) > 32 or not re.fullmatch(r"[0-9.]+:[0-9]+", value):
        reject("numeric_tcp_endpoint_required")
    address, port = value.rsplit(":", 1)
    try:
        ip = ipaddress.IPv4Address(address)
    except ipaddress.AddressValueError:
        reject("numeric_tcp_endpoint_required")
    number = int(port)
    if not 1 <= number <= 65535 or str(ip) != address or str(number) != port:
        reject("numeric_tcp_endpoint_required")
    return str(ip), number


def network_policy(relay_endpoint=None, provider_endpoints=()):
    providers = list(provider_endpoints)
    if len(providers) > 4 or len(set(providers)) != len(providers):
        reject("provider_endpoint_bounds")
    if relay_endpoint is not None and tcp_endpoint(relay_endpoint)[0] != "127.0.0.1":
        reject("relay_loopback_required")
    for endpoint in providers:
        address, port = tcp_endpoint(endpoint)
        ip = ipaddress.IPv4Address(address)
        if not ip.is_global or ip.is_multicast or ip.is_reserved or port != 443:
            reject("provider_public_ipv4_tls_port_required")
    return {"relay_endpoint": relay_endpoint, "provider_endpoints": sorted(providers),
            "dns_allowed": False, "unix_sockets_allowed": False,
            "provider_identity_verified": False}


def snapshot(source):
    """Copy the public package only; no repository, tests, caches, or symlinks."""
    source = safe_path(source)
    required = ["SKILL.md", "agents/openai.yaml", "scripts/install.py",
                "scripts/producer-isolation-preflight.py",
                "scripts/producer-isolation-probe.c"]
    references = source / "references"
    if references.is_symlink() or not references.is_dir():
        reject("public_references_required")
    required.extend(str(p.relative_to(source)) for p in sorted(references.glob("*.md")))
    result = {}
    total = 0
    for relative in required:
        path = source / relative
        if any(p.is_symlink() for p in [path, *path.parents[:len(Path(relative).parts)]]):
            reject("public_package_symlink")
        info = path.stat()
        if not stat.S_ISREG(info.st_mode) or info.st_size > 256 * 1024:
            reject("public_package_file_bounds")
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
        with os.fdopen(fd, "rb") as stream:
            data = stream.read(256 * 1024 + 1)
        total += len(data)
        if len(data) > 256 * 1024 or total > 2 * 1024 * 1024:
            reject("public_package_bounds")
        result[relative] = data
    return result


def manifest_files(root):
    result = []
    for path in sorted(root.rglob("*")):
        if path.is_symlink():
            reject("public_package_symlink")
        if path.is_file():
            result.append({"path": str(path.relative_to(root)), "bytes": path.stat().st_size,
                           "sha256": digest(path)})
    return result


def public_layout(root):
    result = []
    allowed_links = {f"project/{entry}/skills/ato-formation-explore"
                     for entry in [".agents", ".claude"]}
    for path in sorted(root.rglob("*")):
        relative = str(path.relative_to(root))
        if path.is_symlink():
            if relative not in allowed_links or path.resolve() != root / "skill":
                reject("public_layout_symlink")
            result.append({"path": relative, "link": os.readlink(path)})
        elif path.is_file():
            result.append({"path": relative, "sha256": digest(path)})
        elif not path.is_dir():
            reject("public_layout_file_type")
    return result


def profile(public, scratch, executables, auth_ipc=False, tcp_endpoints=()):
    """A dedicated Producer policy. Existing Capsule policies stay untouched."""
    read_paths = [safe_path(public), *[safe_path(p) for p in executables]]
    ancestors = {p for path in [*read_paths, safe_path(scratch)]
                 for p in path.parents if str(p) != "/"}
    lines = ["(version 1)", "(deny default)", "(allow process-fork)",
             "(allow signal (target self))", "(allow sysctl-read)",
             # libSystem ignition requires opening the root directory. A
             # literal grants no access to child paths (same as lib/sandbox).
             "(allow file-read* (literal \"/\"))"]
    lines.extend(f"(allow file-read-metadata (literal {quoted(p)}))"
                 for p in sorted(ancestors))
    lines.extend(f"(allow process-exec (literal {quoted(p)}))" for p in executables)
    lines.append(f"(allow file-read* (subpath {quoted(public)}))")
    lines.extend(f"(allow file-read* (literal {quoted(p)}))" for p in executables)
    lines.append(f"(allow file-read* file-write* (subpath {quoted(scratch)}))")
    lines.extend(["(allow file-read* (subpath \"/System/Library\") (subpath \"/usr/lib\")",
                  "  (subpath \"/private/var/db/dyld\") (literal \"/dev/null\")",
                  "  (literal \"/dev/random\") (literal \"/dev/urandom\") (subpath \"/dev/fd\"))",
                  "(allow file-write* (literal \"/dev/null\") (subpath \"/dev/fd\"))"])
    if auth_ipc:
        lines.extend(f"(allow mach-lookup (global-name {json.dumps(name)}))"
                     for name in MACH_AUTH)
    if len(tcp_endpoints) > 5 or len(set(tcp_endpoints)) != len(tcp_endpoints):
        reject("profile_endpoint_bounds")
    for endpoint in sorted(tcp_endpoints):
        tcp_endpoint(endpoint)
        lines.append(f"(allow network-outbound (remote tcp {json.dumps(endpoint)}))")
    # No unrestricted networking, inbound, DNS, UDP, Unix socket, arbitrary exec,
    # HOME/keychain file, or /tmp rule. Endpoint ACLs do not validate TLS identity.
    return "\n".join(lines) + "\n"


def prepare(output, measurement_id, execution_pin, mcp_binary, codex_binary,
            codex_version, claude_binary, claude_version, source=SKILL,
            relay_endpoint=None, provider_endpoints=()):
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]{1,95}", measurement_id):
        reject("measurement_id_required")
    if not re.fullmatch(r"[a-f0-9]{40}", execution_pin):
        reject("exact_execution_pin_required")
    codex_supported = version_tuple(codex_version) >= (0, 99, 0)
    version_tuple(claude_version)
    network = network_policy(relay_endpoint, provider_endpoints)
    endpoints = ([network["relay_endpoint"]] if network["relay_endpoint"] else []) + network["provider_endpoints"]
    selected = {"mcp": binary(mcp_binary), "codex": binary(codex_binary),
                "claude-code": binary(claude_binary)}
    files = snapshot(source)
    output = Path(output).absolute()
    if output.exists() or output.is_symlink():
        reject("fresh_output_required")
    parent = safe_path(output.parent)
    if (parent != output.parent or not parent.is_relative_to(SKILL.parents[1])
            or ".tmp" not in parent.relative_to(SKILL.parents[1]).parts):
        reject("workspace_tmp_output_required")
    output.mkdir(mode=0o700)
    public = output / "public"
    package = public / "skill"
    for relative, data in files.items():
        path = package / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        path.chmod(0o444)
    if snapshot(source) != files:
        reject("public_package_changed_during_copy")
    project = public / "project"
    for entry in [".agents", ".claude"]:
        destination = project / entry / "skills" / "ato-formation-explore"
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.symlink_to(os.path.relpath(package, destination.parent),
                               target_is_directory=True)
    (public / "fixed-mcp.json").write_text(json.dumps({
        "server": "formation", "transport": "stdio", "tools": MCP_TOOLS,
        "binary": selected["mcp"], "connection": "not supplied", "network_policy": network,
    }, sort_keys=True) + "\n")
    (output / "scratch").mkdir(mode=0o700)
    for name, item in selected.items():
        if name == "mcp":
            continue
        text = profile(public, output / "scratch",
                       [Path(item["path"]), Path(selected["mcp"]["path"])], auth_ipc=True,
                       tcp_endpoints=endpoints)
        (output / f"{name}.prepared.sb").write_text(text)
    records = manifest_files(package)
    package_sha = hashlib.sha256(canonical(records)).hexdigest()
    result = {
        "schema": "ato.formation-producer-isolation-preflight/1",
        "measurement_id": measurement_id, "execution_pin": execution_pin,
        "public_package_sha256": package_sha, "files": records,
        "public_layout": public_layout(public),
        "binaries": selected, "inventory_source": "operator-supplied exact local --version",
        "versions": {"codex": codex_version, "claude-code": claude_version},
        "codex_source_feature_floor": "0.99.0", "codex_version_eligible": codex_supported,
        "mcp_tools": MCP_TOOLS, "mcp_connection_supplied": False,
        "network_policy": network,
        "os_fixture_protocol": 2,
        "profiles": {name: digest(output / f"{name}.prepared.sb")
                     for name in ["codex", "claude-code"]},
        "native_acceptance_ready": False,
        "gates": ["approved_plan", "native_skill_discovery", "native_tool_inventory",
                  "native_read_write_exec_negative", "native_auth_ipc_without_tool_access",
                  "fixed_mcp_descriptor_binding", "native_provider_egress",
                  "owner_approved_provider_endpoint_identity", "native_provider_dns_tls"],
        "credential_files_allowed": False, "global_config_changed": False,
        "native_agent_or_auth_invoked": False,
    }
    (output / "manifest.json").write_text(json.dumps(result, indent=2) + "\n")
    return result


def verify(root):
    root = safe_path(root)
    for name in ["public", "public/skill", "public/project", "scratch"]:
        path = root / name
        if path.is_symlink() or not path.is_dir():
            reject("package_directory_changed")
    info = json.loads((root / "manifest.json").read_text())
    if info["schema"] != "ato.formation-producer-isolation-preflight/1":
        reject("package_schema")
    records = manifest_files(root / "public/skill")
    if records != info["files"] or hashlib.sha256(canonical(records)).hexdigest() != info["public_package_sha256"]:
        reject("public_package_changed")
    if public_layout(root / "public") != info["public_layout"]:
        reject("public_layout_changed")
    for name, item in info["binaries"].items():
        if binary(item["path"]) != item:
            reject("fixed_binary_changed")
    for name, expected in info["profiles"].items():
        if digest(root / f"{name}.prepared.sb") != expected:
            reject("profile_changed")
    for entry in [".agents", ".claude"]:
        destination = root / "public/project" / entry / "skills/ato-formation-explore"
        if not destination.is_symlink() or destination.resolve() != root / "public/skill":
            reject("skill_link_changed")
    expected = {"server": "formation", "transport": "stdio", "tools": MCP_TOOLS,
                "binary": info["binaries"]["mcp"], "connection": "not supplied"}
    if "network_policy" in info:
        network = info["network_policy"]
        if network_policy(network["relay_endpoint"], network["provider_endpoints"]) != network:
            reject("network_policy_changed")
        endpoints = ([network["relay_endpoint"]] if network["relay_endpoint"] else []) + network["provider_endpoints"]
        for name in ["codex", "claude-code"]:
            generated = profile(root / "public", root / "scratch",
                                [Path(info["binaries"][name]["path"]), Path(info["binaries"]["mcp"]["path"])],
                                auth_ipc=True, tcp_endpoints=endpoints)
            if (root / f"{name}.prepared.sb").read_text() != generated:
                reject("profile_policy_mismatch")
        expected["network_policy"] = network
    if json.loads((root / "public/fixed-mcp.json").read_text()) != expected:
        reject("fixed_mcp_changed")
    return info


def probe(root):
    if sys.platform != "darwin":
        reject("native_macos_required")
    root = safe_path(root)
    info = verify(root)
    if info.get("os_fixture_protocol") != 2:
        reject("new_fixture_package_required")
    if (root / "probe").exists() or (root / "os-fixture.json").exists():
        reject("fresh_probe_required")
    work = root / "probe"
    work.mkdir(mode=0o700)
    private = work / "private"
    private.mkdir(mode=0o700)
    values = []
    for name in CANARIES:
        value = secrets.token_hex(32).encode()
        values.append(value)
        path = private / name
        path.write_bytes(value)
        path.chmod(0o600)
    executable = private / "direct-launch"
    executable.write_text("#!/bin/sh\nexit 0\n")
    executable.chmod(0o700)
    escape = root / "public/canary-escape"
    escape.symlink_to(private / "owner-credential")
    source = root / "public/skill/scripts/producer-isolation-probe.c"
    runner = work / "runner"
    clean_env = {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "TMPDIR": str(root / "scratch"),
                 "LANG": "C", "LC_ALL": "C"}
    compiled = subprocess.run(["/usr/bin/clang", "-Wall", "-Wextra", "-Werror", "-O2",
                               str(source), "-o", str(runner)], env=clean_env,
                              capture_output=True, timeout=60)
    if compiled.returncode != 0:
        reject("fixture_compile_failed")
    text = profile(root / "public", root / "scratch", [runner], auth_ipc=True)
    policy = work / "fixture.sb"
    policy.write_text(text)
    sockets = []
    unix_paths = []
    def tcp(address, port=0, family=socket.AF_INET):
        endpoint = socket.socket(family, socket.SOCK_STREAM)
        sockets.append(endpoint)
        endpoint.bind((address, port))
        endpoint.listen()
        return endpoint.getsockname()[1]
    try:
        relay_port = tcp("127.0.0.1")
        provider_port = tcp("127.0.0.2")
        wrong_port = tcp("127.0.0.1")
        wrong_provider_port = tcp("127.0.0.2")
        tcp("127.0.0.2", relay_port)
        tcp("127.0.0.1", provider_port)
        ipv6_port = tcp("::1", family=socket.AF_INET6)
        udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sockets.append(udp)
        udp.bind(("127.0.0.1", relay_port))
        fixture_endpoints = [f"127.0.0.1:{relay_port}", f"127.0.0.2:{provider_port}"]
        text = profile(root / "public", root / "scratch", [runner], auth_ipc=True,
                       tcp_endpoints=fixture_endpoints)
        policy.write_text(text)
        # Relative paths avoid macOS's short Unix socket path limit. Each denied
        # destination is actually listening; ECONNREFUSED/ENOENT is not proof.
        previous_directory = Path.cwd()
        try:
            os.chdir(root)
            for name in ["docker.sock", "owner-control.sock"]:
                endpoint = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                sockets.append(endpoint)
                path = "scratch/" + name
                endpoint.bind(path)
                endpoint.listen()
                unix_paths.append(root / path)
        finally:
            os.chdir(previous_directory)
        completed = subprocess.run(["/usr/bin/sandbox-exec", "-f", str(policy), str(runner),
                                    str(root / "public/skill/SKILL.md"), str(private),
                                    str(root / "scratch"), str(escape), "../../scratch/docker.sock",
                                    str(relay_port), str(provider_port), str(wrong_port),
                                    str(wrong_provider_port), "../../scratch/owner-control.sock",
                                    str(ipv6_port)], env=clean_env, close_fds=True,
                                   cwd=root / "public/project", capture_output=True, timeout=30)
    finally:
        for endpoint in sockets:
            endpoint.close()
        escape.unlink()
        for path in unix_paths:
            if path.exists():
                path.unlink()
    if any(value in completed.stdout or value in completed.stderr for value in values):
        reject("fixture_canary_exposure")
    def failure(code, checks=None):
        failure = {"schema": "ato.formation-producer-isolation-os-fixture-failure/1",
                   "measurement_id": info["measurement_id"], "execution_pin": info["execution_pin"],
                   "public_package_sha256": info["public_package_sha256"],
                   "code": code, "fixture_returncode": completed.returncode,
                   "stdout_bytes": len(completed.stdout), "stderr_bytes": len(completed.stderr),
                   "native_agent_or_auth_invoked": False, "native_acceptance_ready": False}
        if checks is not None:
            failure["checks"] = {name: checks.get(name) is True for name in OS_CHECKS}
        (root / "os-fixture-failure.json").write_text(json.dumps(failure, indent=2) + "\n")
        reject(code)
    if completed.returncode != 0 or len(completed.stdout) > 8192:
        failure("kernel_fixture_failed")
    try:
        checks = json.loads(completed.stdout)
    except (ValueError, UnicodeError):
        failure("fixture_result_invalid")
    if not isinstance(checks, dict) or set(checks) != set(OS_CHECKS):
        failure("fixture_result_invalid")
    if any(value is not True for value in checks.values()):
        failure("kernel_boundary_not_enforced", checks)
    for name, value in zip(CANARIES, values):
        if (private / name).read_bytes() != value:
            reject("private_fixture_changed")
    verify(root)
    result = {
        "schema": "ato.formation-producer-isolation-os-fixture/1",
        "measurement_id": info["measurement_id"], "execution_pin": info["execution_pin"],
        "public_package_sha256": info["public_package_sha256"],
        "environment": {"platform": platform.platform(), "machine": platform.machine()},
        "runner_sha256": digest(runner), "profile_sha256": digest(policy),
        "checks": checks, "fixture_canary_output_exposure": False,
        "fixture_tcp_endpoints": fixture_endpoints,
        "configured_native_policy": info.get("network_policy"),
        "provider_fixture_is_loopback_substitute": True,
        "public_provider_reached": False,
        "relay_descriptor_or_owner_connection_read": False,
        "inherited_extra_descriptors": False,
        "captured_output_sha256": hashlib.sha256(completed.stdout + completed.stderr).hexdigest(),
        "captured_output_bytes": len(completed.stdout) + len(completed.stderr),
        "auth_IPC_rules_prepared": MACH_AUTH, "native_auth_IPC_invoked": False,
        "native_agent_tool_inventory_measured": False, "native_acceptance_ready": False,
        "Source_or_owner_credentials_read": False, "Formation_searches": 0,
        "Formation_inference_dispatches": 0, "paid_API_calls": 0, "real_Runtime_attempts": 0,
        "agent_internal_LLM_calls": "unknown", "agent_internal_cost": "unknown",
    }
    (root / "os-fixture.json").write_text(json.dumps(result, indent=2) + "\n")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="operation", required=True)
    preparation = commands.add_parser("prepare")
    preparation.add_argument("--output", type=Path, required=True)
    preparation.add_argument("--measurement-id", required=True)
    preparation.add_argument("--execution-pin", required=True)
    for name in ["mcp", "codex", "claude"]:
        preparation.add_argument(f"--{name}-binary", type=Path, required=True)
    preparation.add_argument("--codex-version", required=True)
    preparation.add_argument("--claude-version", required=True)
    preparation.add_argument("--relay-endpoint", help="Owner-selected numeric 127.0.0.1:PORT; no descriptor is read")
    preparation.add_argument("--provider-endpoint", action="append", default=[],
                             help="Owner-approved public numeric IPv4:443, at most four; no DNS or automatic lookup")
    for name in ["verify", "probe"]:
        command = commands.add_parser(name)
        command.add_argument("--package", type=Path, required=True)
    args = parser.parse_args()
    try:
        if args.operation == "prepare":
            result = prepare(args.output, args.measurement_id, args.execution_pin,
                             args.mcp_binary, args.codex_binary, args.codex_version,
                             args.claude_binary, args.claude_version,
                             relay_endpoint=args.relay_endpoint, provider_endpoints=args.provider_endpoint)
        elif args.operation == "probe":
            result = probe(args.package)
        else:
            result = verify(args.package)
    except Rejected as error:
        parser.exit(1, json.dumps({"status": "rejected", "code": str(error)}) + "\n")
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
        # Never serialize a private path, file body, subprocess output, or exception.
        parser.exit(1, '{"status":"rejected","code":"preflight_failed"}\n')
    print(json.dumps({"status": "prepared" if args.operation != "probe" else "os_fixture_pass",
                      "measurement_id": result["measurement_id"],
                      "public_package_sha256": result["public_package_sha256"],
                      "native_acceptance_ready": False}, sort_keys=True))


if __name__ == "__main__":
    main()
