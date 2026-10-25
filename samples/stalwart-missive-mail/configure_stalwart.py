#!/usr/bin/env python3
"""Idempotently configure the Stalwart state used by this acceptance fixture."""

from __future__ import annotations

import argparse
import base64
import json
import os
import ssl
import urllib.request
from pathlib import Path


CAPABILITY = "urn:stalwart:jmap"


def required_env(name: str) -> str:
    value = os.environ.get(name, "")
    if not value:
        raise SystemExit(f"{name} is required")
    return value


class Registry:
    def __init__(self, url: str, username: str, password: str) -> None:
        self.url = url
        token = base64.b64encode(f"{username}:{password}".encode()).decode()
        self.headers = {"Authorization": f"Basic {token}", "Content-Type": "application/json"}

    def call(self, object_type: str, operation: str, arguments: dict[str, object]) -> dict:
        body = json.dumps(
            {
                "using": ["urn:ietf:params:jmap:core", CAPABILITY],
                "methodCalls": [[f"x:{object_type}/{operation}", arguments, "c0"]],
            }
        ).encode()
        request = urllib.request.Request(self.url, data=body, headers=self.headers)
        with urllib.request.urlopen(request, timeout=20, context=ssl.create_default_context()) as response:
            payload = json.load(response)
        method, result, _call_id = payload["methodResponses"][0]
        if method == "error" or "notCreated" in result or "notUpdated" in result:
            raise RuntimeError(json.dumps(payload, separators=(",", ":")))
        return result

    def all(self, object_type: str) -> list[dict]:
        return self.call(object_type, "get", {})["list"]

    def create(self, object_type: str, key: str, value: dict[str, object]) -> dict:
        return self.call(object_type, "set", {"create": {key: value}})["created"][key]

    def update(self, object_type: str, object_id: str, value: dict[str, object]) -> None:
        self.call(object_type, "set", {"update": {object_id: value}})


def by_name(objects: list[dict], name: str) -> dict | None:
    return next((item for item in objects if item.get("name") == name), None)


def ensure_user(registry: Registry, domain_id: str, password: str) -> str:
    user = by_name(registry.all("Account"), "alice")
    if user is None:
        user = registry.create(
            "Account",
            "alice",
            {
                "@type": "User",
                "name": "alice",
                "domainId": domain_id,
                "credentials": {
                    "0": {"@type": "Password", "secret": password},
                },
                "roles": {"@type": "User"},
                "permissions": {"@type": "Inherit"},
            },
        )
    return str(user["id"])


def ensure_relay(registry: Registry) -> None:
    route = by_name(registry.all("MtaRoute"), "ato-fixed-relay")
    desired = {
        "@type": "Relay",
        "name": "ato-fixed-relay",
        "description": "Ato fixed TLS relay transport Adapter",
        "address": "relay",
        "port": 2525,
        "protocol": "smtp",
        "implicitTls": False,
        "allowInvalidCerts": False,
        "authUsername": required_env("RELAY_AUTH_USERNAME"),
        "authSecret": {
            "@type": "EnvironmentVariable",
            "variableName": "ATO_BINDING_RELAY_AUTH",
        },
    }
    if route is None:
        registry.create("MtaRoute", "ato-fixed-relay", desired)
    else:
        immutable_name = desired.pop("name")
        assert immutable_name == route["name"]
        registry.update("MtaRoute", str(route["id"]), desired)

    strategies = registry.all("MtaOutboundStrategy")
    strategy = {
        "route": {
            "match": {
                "0": {
                    "if": "is_local_domain(rcpt_domain)",
                    "then": "'local'",
                }
            },
            "else": "'ato-fixed-relay'",
        },
    }
    if strategies:
        registry.update("MtaOutboundStrategy", str(strategies[0]["id"]), strategy)
    else:
        registry.create("MtaOutboundStrategy", "singleton", strategy)


def ensure_certificate(registry: Registry, certificate_path: Path, key_path: Path) -> None:
    desired = {
        "certificate": {"@type": "Text", "value": certificate_path.read_text()},
        "privateKey": {"@type": "Text", "secret": key_path.read_text()},
    }
    existing = registry.all("Certificate")
    if existing:
        registry.update("Certificate", str(existing[0]["id"]), desired)
    else:
        registry.create("Certificate", "fixture-tls", desired)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--url", default="http://127.0.0.1:8080/jmap/")
    parser.add_argument("--certificate", type=Path, required=True)
    parser.add_argument("--private-key", type=Path, required=True)
    args = parser.parse_args()
    registry = Registry(args.url, required_env("STALWART_ADMIN_USER"), required_env("STALWART_ADMIN_PASSWORD"))
    accounts = registry.all("Account")
    admin = by_name(accounts, "admin")
    if admin is None:
        raise SystemExit("bootstrapped admin account not found")
    ensure_user(registry, str(admin["domainId"]), required_env("STALWART_USER_PASSWORD"))
    ensure_relay(registry)
    ensure_certificate(registry, args.certificate, args.private_key)
    print("Stalwart fixture configuration is ready")


if __name__ == "__main__":
    main()
