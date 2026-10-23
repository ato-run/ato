#!/usr/bin/env python3
"""Actual Agent PTY + common Gateway acceptance INSIDE a private Linux sandbox.

The caller provides isolated network/PID/mount namespaces and an empty work root.
No application Contract is inferred or issued by this transport test.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import select
import socket
import subprocess
import time

spec = importlib.util.spec_from_file_location("pixel_acceptance", Path(__file__).with_name("linux-pixel.py"))
pixel = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pixel)


def until(predicate, message, seconds=6):
    deadline = time.monotonic() + seconds
    while not predicate():
        assert time.monotonic() < deadline, message
        time.sleep(0.02)


def control(client, **value):
    client.sendall(json.dumps(value).encode(), opcode=1)


def ready(client):
    opcode, payload = client.read_message()
    assert opcode == 1
    result = json.loads(payload)
    assert result["type"] == "ready", result
    return result


def output_until(client, expected):
    result = bytearray()
    deadline = time.monotonic() + 6
    while expected not in result:
        assert time.monotonic() < deadline, "PTY operation output timed out"
        opcode, payload = client.read_message()
        assert opcode == 2, (opcode, payload)
        result.extend(payload)
        assert len(result) <= 65536, "unbounded terminal output"
        control(client, type="ack", bytes=len(payload))
    return bytes(result)


def stopped(port):
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=0.2):
            return False
    except OSError:
        return True


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--agent", type=Path, required=True)
    parser.add_argument("--gateway", type=Path, required=True)
    parser.add_argument("--work-root", type=Path, required=True)
    args = parser.parse_args()
    root = args.work_root.resolve()
    root.mkdir(mode=0o700, parents=False, exist_ok=False)
    endpoint = root / "term.sock"
    config = root / "supervisor.json"
    config.write_text(json.dumps({"stdio_mode": "pty", "cwd": str(root), "cmd": [
        "/bin/sh", "-c", "stty -echo; echo $$ > child.pid; printf 'INITIAL-PROMPT\\n'; "
        "while IFS= read -r line; do case \"$line\" in "
        "size) stty size;; *) printf 'EXECUTED:%s\\n' \"$line\";; esac; done"]}))
    environment = dict(os.environ, ATO_GUEST_AGENT_MODE="stdio", ATO_SUPERVISOR_CONFIG=str(config),
        ATO_BINDINGS_ROOT=str(root / "bindings"), ATO_GUEST_AGENT_TERMINAL_UNIX_SOCKET=str(endpoint))
    execution = {"run_id": "run-native-acceptance", "lease_id": "lease-native-acceptance",
                 "runner_id": "runner-native-acceptance"}
    key = os.urandom(32).hex()
    def token(**changes):
        return pixel.assertion(key, execution=execution, surface="native-terminal",
                               kid="ato.runner-surface@1", **changes)
    gateway = None
    client = None
    started = time.monotonic()
    with (root / "process.log").open("wb") as log:
        agent = subprocess.Popen([str(args.agent.resolve())], stdin=subprocess.PIPE,
            stdout=log, stderr=log, env=environment)
        try:
            until(lambda: endpoint.exists() and (root / "child.pid").exists(), "Agent did not start")
            child_pid = int((root / "child.pid").read_text())
            def start_gateway(idle):
                process = subprocess.Popen([str(args.gateway.resolve())], stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE, stderr=log, text=True)
                value = {"execution": execution, "session_id": "native-acceptance",
                    "surface_id": "native-terminal", "listen_addr": "127.0.0.1:16000",
                    "connect_url": "http://127.0.0.1:16000/", "origin": "http://localhost:5173",
                    "assertion_keys": {"ato.runner-surface@1": key}, "idle_timeout_secs": idle,
                    "readiness_timeout_secs": 5,
                    "target": {"kind": "terminal", "firecracker_vsock_uds": str(endpoint)}}
                process.stdin.write(json.dumps(value) + "\n")
                process.stdin.flush()
                assert select.select([process.stdout], [], [], 8)[0], "Gateway readiness timed out"
                value = json.loads(process.stdout.readline())
                assert value["surface_ready"] and value["kind"] == "terminal"
                assert value["contract_verified"] is False
                return process
            gateway = start_gateway(30)
            rejected = [token(expired=True), token(session="foreign-session")]
            for field in execution:
                rejected.append(pixel.assertion(key, execution=dict(execution, **{field: "foreign"}),
                    surface="native-terminal", kid="ato.runner-surface@1"))
            rejected.append(pixel.assertion(key, surface="native-terminal", kid="ato.runner-surface@1"))
            for signed in rejected:
                try:
                    candidate = pixel.WebSocketSocket(16000, signed, "ato.terminal.v1")
                    candidate.close()
                    raise AssertionError("invalid execution assertion admitted")
                except pixel.UpgradeRejected as error:
                    assert "401" in str(error), str(error)
            accepted = token()
            client = pixel.WebSocketSocket(16000, accepted, "ato.terminal.v1")
            initial = ready(client)
            control(client, type="resize", cols=80, rows=24)
            output_until(client, b"INITIAL-PROMPT\r\n")
            ready_ms = round((time.monotonic() - started) * 1000)
            client.sendall(b"hello\n")
            output_until(client, b"EXECUTED:hello\r\n")
            control(client, type="resize", cols=120, rows=40)
            client.sendall(b"size\n")
            output_until(client, b"40 120\r\n")
            client.close()
            client = None
            time.sleep(0.1)
            try:
                candidate = pixel.WebSocketSocket(16000, accepted, "ato.terminal.v1")
                candidate.close()
                raise AssertionError("replayed assertion admitted")
            except pixel.UpgradeRejected as error:
                assert "401" in str(error)
            client = pixel.WebSocketSocket(16000, token(), "ato.terminal.v1")
            reconnect = ready(client)
            assert (reconnect["cols"], reconnect["rows"]) == (120, 40), reconnect
            control(client, type="resize", cols=120, rows=40)
            client.sendall(b"reconnected\n")
            output_until(client, b"EXECUTED:reconnected\r\n")
            # Keep a viewer active while the owner stops the common gateway.
            gateway.stdin.close()
            assert gateway.wait(timeout=5) == 0
            until(lambda: stopped(16000), "Gateway listener survived owner EOF")
            client.close()
            client = None
            gateway = start_gateway(2)
            assert gateway.wait(timeout=6) == 0
            stopped_event = json.loads(gateway.stdout.readline())
            assert stopped_event["surface_stopped"] and stopped_event["idle_expired"]
            until(lambda: stopped(16000), "Gateway listener survived idle timeout")
            agent.stdin.close()
            assert agent.wait(timeout=6) == 0
            assert not endpoint.exists(), "private socket survived Agent EOF"
            try:
                os.kill(child_pid, 0)
                raise AssertionError("PTY workload survived Agent EOF")
            except ProcessLookupError:
                pass
            result = {"surface_ready_ms": ready_ms, "initial_output_preserved": True,
                "actual_workload_input": True, "resize": [120, 40], "reconnect": True,
                "scoped_auth_rejections": len(rejected), "replay_rejected": True,
                "gateway_owner_cleanup": True, "gateway_idle_cleanup": True,
                "producer_owner_cleanup": True, "contract_verified": False,
                "initial_size": [initial["cols"], initial["rows"]]}
            (root / "acceptance.json").write_text(json.dumps(result, indent=2) + "\n")
            print(json.dumps(result))
        finally:
            if client is not None:
                client.close()
            for process in (gateway, agent):
                if process is not None and process.poll() is None:
                    if process.stdin and not process.stdin.closed:
                        process.stdin.close()
                    try:
                        process.wait(timeout=6)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=3)


if __name__ == "__main__":
    main()
