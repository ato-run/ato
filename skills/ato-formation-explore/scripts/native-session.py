#!/usr/bin/env python3
"""Native clients of the fixed Rust MCP Session; never a proposal engine.

Owner startup runs outside the model region. Codex Code Mode has a separate
deny-default process; Claude has no built-in tools, hooks or extra MCP servers.
Only the public Skill and scoped relay descriptor enter the native project.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import platform
import selectors
import re
import signal
import socket
import subprocess
import time

SKILL = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "producer_isolation", SKILL / "scripts/producer-isolation-preflight.py")
ISOLATION = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ISOLATION)
TOOLS = ["status", "next", "submit", "cancel"]
PROMPT = ("Use the explicitly invoked ato-formation-explore Skill for the one "
          "already authorized Search bound to the Formation MCP server. Call status "
          "first and reconcile saved responses and unresolved attempts. Get the "
          "frozen input through next, then inspect, propose or decline through submit. "
          "Process exactly one new exchange in this turn, save its response and return. "
          "If Ato reports a terminal or blocked state, report it and return. Do not ask for "
          "credentials or change Source, K, permissions, limits or deadlines. Only "
          "saved Ato evidence determines success. Report unknown agent costs as unknown.")


def write(path, value):
    with path.open("x") as stream:
        json.dump(value, stream, indent=2)
        stream.write("\n")
    path.chmod(0o600)


def toml(value):
    if isinstance(value, dict):
        return "{ " + ", ".join(k + " = " + toml(v) for k, v in value.items()) + " }"
    return json.dumps(value)


def status(mcp, connection):
    request = {"jsonrpc": "2.0", "id": 1, "method": "tools/call",
               "params": {"name": "status", "arguments": {}}}
    result = subprocess.run([str(mcp), "--connection", str(connection)],
                            input=json.dumps(request) + "\n", text=True,
                            capture_output=True, timeout=10, check=True)
    response = json.loads(result.stdout)
    if response.get("result", {}).get("isError") is not False:
        raise ISOLATION.Rejected("session_status_rejected")
    return response["result"]["structuredContent"]


def next_action(view):
    """Wait for a known active attempt, never turn waiting into inference."""
    progress = view.get("progress", {})
    if (view.get("connected") is not True
            or progress.get("status") not in ("pending", "running")
            or progress.get("pause_reason") == "needs_input"):
        return "stop"
    unresolved = progress.get("unresolved_attempts", 0)
    if unresolved:
        attempts = progress.get("attempts", [])
        active = sum(a.get("status") in ("pending", "claimed") for a in attempts)
        return "wait" if active == unresolved and not any(
            a.get("status") == "unknown" for a in attempts) else "stop"
    if view.get("exchange", {}) and view["exchange"].get("response_saved") is True:
        return "wait"
    if view.get("next_operation") == "submit":
        return "submit"
    return "stop" if view.get("next_operation", "").startswith("owner_") else "wait"


def admission(view, binding, agent, version):
    expected = "claude_code" if agent == "claude-code" else agent
    if (not binding.get("agent") or binding["agent"].get("kind") != expected
            or binding["agent"].get("version") != version
            or view.get("search_id") != binding.get("search_id")
            or view.get("configuration_ref") != binding.get("configuration_ref")):
        raise ISOLATION.Rejected("session_agent_binding_mismatch")
    if view.get("connected") is not True or view.get("next_operation") != "submit":
        raise ISOLATION.Rejected("owner_reconciliation_required")
    if (view.get("progress", {}).get("status") not in ("pending", "running")
            or view.get("progress", {}).get("pause_reason") == "needs_input"
            or view.get("progress", {}).get("unresolved_attempts", 0) != 0
            or view.get("exchange", {}).get("response_saved") is not False):
        raise ISOLATION.Rejected("search_not_accepting_inference")
    deadline = min(view["deadline_ms"], view["exchange_deadline_ms"])
    if not isinstance(deadline, int) or deadline <= time.time() * 1000:
        raise ISOLATION.Rejected("deadline_exceeded")
    return view["deadline_ms"]


def public_package(output):
    public = output / "public"
    package = public / "skill"
    for relative, data in ISOLATION.snapshot(SKILL).items():
        destination = package / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(data)
        destination.chmod(0o444)
    project = public / "project"
    for entry in (".agents", ".claude"):
        link = project / entry / "skills/ato-formation-explore"
        link.parent.mkdir(parents=True, exist_ok=True)
        link.symlink_to(os.path.relpath(package, link.parent), target_is_directory=True)
    return public, project


def narrowed_catalog(path, model):
    if path.stat().st_size > 4 * 1024 * 1024:
        raise ISOLATION.Rejected("model_catalog_bounds")
    catalog = json.loads(path.read_text())
    selected = [entry for entry in catalog["models"] if entry.get("slug") == model]
    if len(selected) != 1:
        raise ISOLATION.Rejected("exact_model_metadata_required")
    model_info = dict(selected[0])
    model_info.update(shell_type="disabled", apply_patch_tool_type=None,
                      experimental_supported_tools=[])
    return {"models": [model_info]}


def auth_host_profile(public, home, native, mcp, selected_socket, auth_file=None):
    profile = ISOLATION.profile(public, home, [native, mcp],
                                auth_ipc=True, relay_socket=selected_socket)
    lines = [profile,
             '(allow network-outbound (remote tcp "*:443") (remote udp "*:53"))',
             '(allow mach-lookup (global-name "com.apple.mDNSResponder")',
             ' (global-name "com.apple.trustd.agent") (global-name "com.apple.trustd")',
             ' (global-name "com.apple.cfprefsd.agent") (global-name "com.apple.cfprefsd.daemon")',
             ' (global-name "com.apple.SystemConfiguration.configd")',
             ' (global-name "com.apple.securityd.xpc"))',
             '(allow file-read* (literal "/private/etc/resolv.conf") (literal "/private/etc/hosts")',
             ' (literal "/private/etc/ssl/cert.pem")',
             ' (literal "/Library/Preferences/com.apple.networkd.plist")',
             ' (subpath "/System/Library/Keychains") (literal "/Library/Keychains/System.keychain")',
             ' (subpath "/etc/codex") (subpath "/private/etc/codex")',
             ' (subpath "/Library/Managed Preferences"))',
             '(allow file-read-metadata (literal "/Library") (literal "/Library/Preferences")',
             ' (literal "/Library/Keychains") (literal "/var") (literal "/System/Cryptexes/App")',
             ' (literal "/System/Cryptexes/OS") (literal "/private/var/run")',
             ' (literal "/private/var/run/mDNSResponder"))',
             '(allow network-outbound (remote unix-socket (literal "/private/var/run/mDNSResponder")))',
             '(allow ipc-posix-shm-read-data (ipc-posix-name "apple.shm.notification_center")',
             ' (ipc-posix-name "apple.cfprefs.' + str(os.getuid()) + 'v1")',
             ' (ipc-posix-name "apple.cfprefs.daemonv1"))']
    if auth_file:
        lines.append('(allow file-read* (literal ' + json.dumps(str(auth_file)) + '))')
        lines.extend('(allow file-read-metadata (literal ' + json.dumps(str(p)) + '))'
                     for p in auth_file.parents)
    return "\n".join(lines) + "\n"


class NativeFrames:
    """Read pipe frames without buffered readline/select deadlocks."""
    def __init__(self, stream):
        self.descriptor = stream.fileno()
        self.selector = selectors.DefaultSelector()
        self.selector.register(self.descriptor, selectors.EVENT_READ)
        self.pending = b""

    def receive(self, end):
        while time.time() < end:
            if b"\n" in self.pending:
                line, self.pending = self.pending.split(b"\n", 1)
                return json.loads(line)
            if not self.selector.select(min(1, max(0, end - time.time()))):
                continue
            chunk = os.read(self.descriptor, 65536)
            if not chunk:
                return None
            self.pending += chunk
            if len(self.pending.split(b"\n", 1)[0]) > 4 * 1024 * 1024:
                raise ISOLATION.Rejected("native_frame_bounds")
        return None


class AppServer:
    def __init__(self, process, events):
        self.process, self.events = process, events
        self.frames = NativeFrames(process.stdout)
        self.sequence = 0
        self.methods = {}
        self.mcp_status = {}

    def send(self, method, params, request=False):
        packet = {"method": method, "params": params}
        if request:
            self.sequence += 1
            packet["id"] = self.sequence
            self.methods[self.sequence] = method
        self.process.stdin.write((json.dumps(packet) + "\n").encode())
        self.process.stdin.flush()
        return packet.get("id")

    def receive(self, predicate, end):
        while time.time() < end:
            packet = self.frames.receive(end)
            if packet is None:
                break
            if packet.get("method") == "mcpServer/startupStatus/updated":
                state = packet.get("params", {})
                self.mcp_status[state.get("name")] = state.get("status")
            recorded = packet
            if self.methods.get(packet.get("id")) == "account/read":
                account = packet.get("result", {}).get("account")
                recorded = {"id": packet.get("id"), "account_available": bool(account)}
            self.events.write(json.dumps(recorded) + "\n")
            self.events.flush()
            if "id" in packet and "method" in packet:
                # No tool/permission widening through interactive server requests.
                self.process.stdin.write((json.dumps({"id": packet["id"], "error": {
                    "code": -32601, "message": "Interactive extension unavailable"}}) + "\n").encode())
                self.process.stdin.flush()
            if predicate(packet):
                return packet
        raise ISOLATION.Rejected("native_response_unavailable")

    def request(self, method, params, end):
        identity = self.send(method, params, True)
        reply = self.receive(lambda p: p.get("id") == identity, min(end, time.time() + 30))
        if "error" in reply:
            raise ISOLATION.Rejected("native_rpc_rejected")
        return reply["result"]


def codex_commands(native, public, descriptor, mcp):
    settings = {"cli_auth_credentials_store": "file",
                "model_catalog_json": str(public / "model-catalog.json"),
                "approval_policy": "never", "web_search": "disabled",
                "agents.enabled": False,
                "features.code_mode_host": {"enabled": True, "disable_in_process_fallback": True},
                "mcp_servers.formation": {"command": str(mcp), "args": ["--relay", str(descriptor)],
                    "enabled_tools": TOOLS, "omit_tools_from": ["code_mode", "deferred"]}}
    for feature in ("shell_tool", "unified_exec", "multi_agent", "multi_agent_v2",
                    "apps", "view_image", "goals", "skill_mcp_dependency_install"):
        settings["features." + feature] = False
    command = [str(native)]
    for key, value in settings.items():
        command += ["-c", key + "=" + toml(value)]
    return command


def stop(process):
    if process.poll() is None:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()


def claude_keychain_token(deadline):
    # Trusted Owner adapter only. No token in argv, files, diagnostics, model
    # input or the MCP environment. Native's documented OAuth env is used in
    # the auth host; its subprocess credential scrub is mandatory below.
    result = subprocess.run(["/usr/bin/security", "find-generic-password", "-s",
                             "Claude Code-credentials", "-w"], stdout=subprocess.PIPE,
                            stderr=subprocess.DEVNULL, timeout=10)
    if result.returncode:
        raise ISOLATION.Rejected("native_keychain_credential_unavailable")
    credentials = json.loads(result.stdout).get("claudeAiOauth", {})
    token = credentials.get("accessToken")
    if not isinstance(token, str) or not token or credentials.get("expiresAt", 0) <= deadline:
        raise ISOLATION.Rejected("native_credential_window_insufficient")
    return token


def run(args):
    if platform.system() != "Darwin":
        raise ISOLATION.Rejected("measured_macos_host_required")
    native, mcp = (Path(value).resolve(strict=True) for value in (args.binary, args.mcp_binary))
    pins = {"agent": ISOLATION.binary(native), "mcp": ISOLATION.binary(mcp)}
    observed_version = subprocess.run([str(native), "--version"], capture_output=True,
                                     text=True, timeout=10, check=True).stdout
    if args.version not in re.findall(r"\b[0-9]+\.[0-9]+\.[0-9]+\b", observed_version):
        raise ISOLATION.Rejected("native_version_mismatch")
    connection = Path(args.connection).resolve(strict=True)
    binding = json.loads(connection.read_text())  # Owner process only; never copied publicly.
    if binding.get("agent", {}).get("model") is not None and binding["agent"]["model"] != args.model:
        raise ISOLATION.Rejected("session_model_binding_mismatch")
    view = status(mcp, connection)
    deadline = admission(view, binding, args.agent, args.version)
    output = Path(args.output).absolute()
    if output.exists() or output.is_symlink() or ".tmp" not in output.parts:
        raise ISOLATION.Rejected("fresh_workspace_tmp_required")
    output.parent.resolve(strict=True)
    output.mkdir(mode=0o700)
    owner, scratch = output / "owner", output / "producer"
    owner.mkdir(mode=0o700); scratch.mkdir(mode=0o700)
    public, project = public_package(output)
    selected = Path(args.socket).absolute()
    if len(str(selected).encode()) > 100 or selected.exists() or selected.is_symlink():
        raise ISOLATION.Rejected("fresh_short_socket_required")
    selected.parent.resolve(strict=True)
    descriptor = public / "relay.json"
    processes = []
    handles = []
    socket_identity = None
    result = {"schema": "ato.formation-native-session-launch/1", "agent": args.agent,
              "version": args.version, "search_id": view["search_id"],
              "configuration_ref": view["configuration_ref"], "deadline_ms": deadline,
              "binaries": pins, "Ato_direct_LLM_calls": 0,
              "internal_LLM_calls": "unknown", "token_usage": "unknown", "cost": "unknown",
              "ordinary_Run_authorized": False, "Search_cancelled_by_launcher": False}

    def launch(command, label, environment=None, stdin=False):
        log = (owner / (label + ".stderr")).open("xb"); handles.append(log)
        process = subprocess.Popen(command, cwd=project, env=environment,
            stdin=subprocess.PIPE if stdin else subprocess.DEVNULL,
            stdout=subprocess.PIPE if stdin else log, stderr=log, start_new_session=True)
        processes.append(process)
        return process

    try:
        broker = launch([str(mcp), "--connection", str(connection), "--publish-relay",
                         str(descriptor), "--relay-socket", str(selected)], "broker")
        end = min(time.time() + 10, deadline / 1000)
        while not descriptor.exists() and broker.poll() is None and time.time() < end:
            time.sleep(.02)
        if not descriptor.exists():
            raise ISOLATION.Rejected("fixed_mcp_broker_unavailable")
        socket_info = selected.stat()
        socket_identity = (socket_info.st_dev, socket_info.st_ino)
        home = owner / "native-home"; home.mkdir(mode=0o700)
        environment = {"PATH": "/usr/bin:/bin", "HOME": str(home), "TMPDIR": str(home),
                       "LANG": "en_US.UTF-8"}
        if args.agent == "codex":
            if args.version != "0.160.0" or not all((args.auth_file, args.model_catalog,
                                                    args.code_mode_host, args.model)):
                raise ISOLATION.Rejected("measured_codex_configuration_required")
            helper = Path(args.code_mode_host).resolve(strict=True)
            pins["code_mode_host"] = ISOLATION.binary(helper)
            auth = Path(args.auth_file).resolve(strict=True)
            (home / "auth.json").symlink_to(auth)
            catalog = narrowed_catalog(Path(args.model_catalog).resolve(strict=True), args.model)
            write(public / "model-catalog.json", catalog)
            with socket.socket() as listener:
                listener.bind(("127.0.0.1", 0)); port = listener.getsockname()[1]
            child_profile = ISOLATION.profile(public, scratch, [helper])
            child_profile += '(allow network-bind (local tcp "localhost:' + str(port) + '"))\n'
            child_profile += '(allow network-inbound (local tcp "localhost:' + str(port) + '"))\n'
            child_file = owner / "model-region.sb"; child_file.write_text(child_profile)
            child_environment = dict(environment, HOME=str(scratch), TMPDIR=str(scratch))
            launch(["/usr/bin/sandbox-exec", "-f", str(child_file), str(helper), "--listen",
                    "grpc://127.0.0.1:" + str(port)], "code-mode", child_environment)
            ready_end = min(time.time() + 5, deadline / 1000)
            while True:
                try:
                    with socket.create_connection(("127.0.0.1", port), timeout=.2):
                        break
                except OSError:
                    if time.time() >= ready_end:
                        raise ISOLATION.Rejected("isolated_code_mode_unavailable")
                    time.sleep(.02)
            host_profile = auth_host_profile(public, home, native, mcp, selected, auth)
            host_profile += '(allow network-outbound (remote tcp "localhost:' + str(port) + '"))\n'
            host_file = owner / "auth-host.sb"; host_file.write_text(host_profile)
            environment["CODEX_HOME"] = str(home)
            command = ["/usr/bin/sandbox-exec", "-f", str(host_file)]
            command += codex_commands(native, public, descriptor, mcp)
            command += ["app-server", "--stdio", "--code-mode-host", "http://127.0.0.1:" + str(port)]
            process = launch(command, "codex", environment, True)
            events = (owner / "native-events.jsonl").open("x"); handles.append(events)
            client = AppServer(process, events)
            end = deadline / 1000
            client.request("initialize", {"clientInfo": {"name": "ato-formation-native-session",
                           "version": "1"}, "capabilities": {"experimentalApi": True}}, end)
            client.send("initialized", {})
            account = client.request("account/read", {"refreshToken": False}, end).get("account")
            if not account:
                raise ISOLATION.Rejected("native_account_unavailable")
            # Never record account email, token or the account/read response.
            result["native_account_available"] = True
            thread = client.request("thread/start", {"cwd": str(project), "model": args.model,
                "approvalPolicy": "never", "sandbox": "read-only", "ephemeral": True}, end)["thread"]["id"]
            if client.mcp_status.get("formation") not in ("ready", "failed"):
                client.receive(lambda event: client.mcp_status.get("formation") in ("ready", "failed"),
                               min(end, time.time() + 20))
            if client.mcp_status.get("formation") != "ready":
                raise ISOLATION.Rejected("native_fixed_mcp_unavailable")
            previous_exchange = None
            turns = 0
            while time.time() < end:
                current = status(mcp, connection)
                result["saved_status"] = current
                action = next_action(current)
                if action == "submit":
                    admission(current, binding, args.agent, args.version)
                    exchange = current["exchange"]
                    if exchange == previous_exchange:
                        raise ISOLATION.Rejected("exchange_not_saved_owner_reconciliation_required")
                    inputs = [{"type": "text", "text": PROMPT}]
                    if turns == 0:
                        inputs.insert(0, {"type": "skill", "name": "ato-formation-explore",
                                          "path": str(public / "skill/SKILL.md")})
                    turn_end = min(end, current["exchange_deadline_ms"] / 1000)
                    client.request("turn/start", {"threadId": thread, "input": inputs}, turn_end)
                    client.receive(lambda event: event.get("method") == "turn/completed", turn_end)
                    previous_exchange = exchange
                    turns += 1
                    result["native_turns_completed"] = turns
                elif action == "stop":
                    break
                else:
                    time.sleep(.5)
            result["native_turn_completed"] = turns > 0
        else:
            if args.version != "2.1.288":
                raise ISOLATION.Rejected("measured_claude_configuration_required")
            # The native auth host uses an existing same-account credential,
            # while the model has four fixed MCP tools and no file/shell tool.
            host_profile = auth_host_profile(public, home, native, mcp, selected)
            host_file = owner / "auth-host.sb"; host_file.write_text(host_profile)
            environment.update(CLAUDE_CONFIG_DIR=str(home), CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC="1",
                CLAUDE_CODE_SUBPROCESS_ENV_SCRUB="1", CLAUDE_CODE_TMPDIR=str(home),
                CLAUDE_CODE_OAUTH_TOKEN=claude_keychain_token(deadline))
            prefix = ["/usr/bin/sandbox-exec", "-f", str(host_file), str(native)]
            authenticated = subprocess.run(prefix + ["auth", "status", "--json"],
                cwd=project, env=environment, capture_output=True, timeout=15, check=True)
            if json.loads(authenticated.stdout).get("loggedIn") is not True:
                raise ISOLATION.Rejected("native_account_unavailable")
            result["native_account_available"] = True
            write(public / "claude-mcp.json", {"mcpServers": {"formation": {
                "command": str(mcp), "args": ["--relay", str(descriptor)]}}})
            write(public / "claude-settings.json", {"disableAllHooks": True,
                "enabledPlugins": {}, "permissions": {
                    "allow": ["mcp__formation__" + name for name in TOOLS],
                    "deny": ["Bash", "Read", "Write", "Edit", "Agent", "WebFetch", "WebSearch"]}})
            command = prefix + ["--strict-mcp-config", "--mcp-config", str(public / "claude-mcp.json"),
                "--settings", str(public / "claude-settings.json"), "--setting-sources", "project",
                "--add-dir", str(public / "skill"), "--tools", "", "--allowedTools",
                ",".join("mcp__formation__" + name for name in TOOLS), "--permission-mode", "dontAsk",
                "--permission-prompts", "none", "--no-session-persistence", "--no-chrome",
                "--output-format", "stream-json", "--verbose"]
            if args.model:
                command += ["--model", args.model]
            command += ["-p", "--input-format", "stream-json"]
            process = launch(command, "claude", environment, True)
            events = (owner / "native-events.jsonl").open("x"); handles.append(events)
            frames = NativeFrames(process.stdout)
            previous_exchange = None
            turns = 0
            end = deadline / 1000
            while time.time() < end:
                current = status(mcp, connection)
                result["saved_status"] = current
                action = next_action(current)
                if action == "submit":
                    admission(current, binding, args.agent, args.version)
                    exchange = current["exchange"]
                    if exchange == previous_exchange:
                        raise ISOLATION.Rejected("exchange_not_saved_owner_reconciliation_required")
                    message = ("/ato-formation-explore " if turns == 0 else "") + PROMPT
                    process.stdin.write((json.dumps({"type": "user", "message": {
                        "role": "user", "content": message}}) + "\n").encode())
                    process.stdin.flush()
                    turn_end = min(end, current["exchange_deadline_ms"] / 1000)
                    completed = False
                    while time.time() < turn_end:
                        event = frames.receive(turn_end)
                        if event is None:
                            break
                        events.write(json.dumps(event) + "\n"); events.flush()
                        if event.get("type") == "system" and event.get("subtype") == "init":
                            tools = event.get("tools", [])
                            if sorted(tools) != sorted("mcp__formation__" + name for name in TOOLS):
                                raise ISOLATION.Rejected("native_tool_inventory_changed")
                            result["observed_model"] = event.get("model")
                        if event.get("type") == "result":
                            if event.get("is_error"):
                                raise ISOLATION.Rejected("native_turn_failed")
                            completed = True
                            break
                    if not completed:
                        raise ISOLATION.Rejected("native_response_unavailable")
                    previous_exchange = exchange
                    turns += 1
                    result["native_turns_completed"] = turns
                elif action == "stop":
                    break
                else:
                    time.sleep(.5)
            result["native_turn_completed"] = turns > 0
        result["saved_status"] = status(mcp, connection)
    except (ISOLATION.Rejected, OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        result["failure_code"] = str(error) if isinstance(error, ISOLATION.Rejected) else "native_launch_failed"
        result["next_operation"] = "owner_reconcile"
        raise
    finally:
        for process in reversed(processes):
            stop(process)
        for handle in handles:
            handle.close()
        # Broker termination is not a Search cancellation or Runtime cleanup.
        if selected.exists():
            info = selected.stat()
            if (info.st_dev, info.st_ino) == socket_identity and selected.is_socket():
                selected.unlink()
        write(owner / "launch-result.json", result)
    return {key: result[key] for key in ("schema", "agent", "version", "search_id",
                                        "deadline_ms", "internal_LLM_calls", "cost")}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--agent", choices=["codex", "claude-code"], required=True)
    for name in ("connection", "output", "socket", "binary", "mcp-binary", "version"):
        parser.add_argument("--" + name, required=True)
    for name in ("auth-file", "model-catalog", "code-mode-host", "model"):
        parser.add_argument("--" + name)
    try:
        print(json.dumps(run(parser.parse_args())))
    except (ISOLATION.Rejected, OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        code = str(error) if isinstance(error, ISOLATION.Rejected) else "native_launch_failed"
        print(json.dumps({"error": code, "next_operation": "owner_reconcile", "Search_reset": False}))
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
