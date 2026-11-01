#!/usr/bin/env python3
"""Apply the D1 staging registry configuration to a running ato-mail Stalwart.

Status: NOT YET EXECUTED. Nothing in this directory can be run offline: the
pinned images are preloaded on the dedicated Runner and no Stalwart instance
exists for local execution. This script is the staging step that S3 hands to S5
(and the staging acceptance run); its listener shapes were derived from the
pinned Stalwart v0.16.22 source (commit 474dd0229cb20cf513036619781ed97bd8073c3f:
crates/registry/src/schema/structs.rs, crates/http/src/request.rs and
crates/common/src/config/server/{listener,tls}.rs). Treat a mismatch found on the
first run as a bug in this script, not in the sample's Contract.

Why the configuration is applied here instead of in stalwart-config.json:
Stalwart v0.16.22 keeps listeners, certificates, domains, accounts and MTA
routes in its dynamic registry, not in the JSON config file. stalwart-config.json
in this directory selects the store backend under the state mount; everything
else is applied through the JMAP registry below, idempotently.

No credential, private key or certificate is committed to this repository. The
certificate pair is installed as *references* to EnvironmentVariable values that
arrive as ato.secret@1 Bindings (ATO_BINDING_MAIL_CERT_PEM /
ATO_BINDING_MAIL_KEY_PEM), so Stalwart parses the pair at load time from the
Run's environment (plan G2 / slice S2).

Required environment:

  STALWART_ADMIN_USER / STALWART_ADMIN_PASSWORD   bootstrapped admin account
  STALWART_USER_PASSWORD                          staging-only test account
  RELAY_AUTH_USERNAME                             Stage 1 relay AUTH username
  STALWART_TEST_DOMAIN         (default mx-test.atousercontent.com)
  STALWART_TEST_USER           (default ato-mail-test)
  STALWART_TRUSTED_PROXY_NETWORKS
      Comma-separated CIDRs of the Runner hop that inserts PROXY protocol v2.
      Default 10.0.0.0/8,172.16.0.0/12,192.168.0.0/16: the whole private space,
      because the Runner-created service network's CIDR is not known when the
      bundle is authored. Narrow it to the Runner's actual network for staging
      and production. Trusting a wider range only lets a peer *inside* the
      service group claim another source address; these listeners are not
      published anywhere except through the Runner's fixed-TCP forwarder.

The relay target, its TLS server name and its CA are sample environment values
in capsule.toml (the transport Adapter reads them). This script only points
Stalwart at the Adapter's internal Port and sets the AUTH username; the secret
itself is the relay_auth Binding, referenced as ATO_BINDING_RELAY_AUTH.
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import ssl
import sys
import urllib.request

CAPABILITY = "urn:stalwart:jmap"

# Binding environment names, fixed by the Application's Binding ids.
CERT_ENV = "ATO_BINDING_MAIL_CERT_PEM"
KEY_ENV = "ATO_BINDING_MAIL_KEY_PEM"
RELAY_SECRET_ENV = "ATO_BINDING_RELAY_AUTH"

# name, port, protocol, use_tls, tls_implicit, trust_proxy
LISTENERS = [
    ("ato-mail-smtp", 25, "smtp", True, False, True),
    ("ato-mail-submissions", 465, "smtp", True, True, True),
    ("ato-mail-submission-starttls", 587, "smtp", True, False, True),
    ("ato-mail-imaps", 993, "imap", True, True, True),
    ("ato-mail-http", 8080, "http", False, False, False),
]

DEFAULT_TRUSTED_PROXY_NETWORKS = "10.0.0.0/8,172.16.0.0/12,192.168.0.0/16"


def required_env(name: str) -> str:
    value = os.environ.get(name, "")
    if not value:
        raise SystemExit(f"{name} is required")
    return value


class Registry:
    """Minimal JMAP registry client for the pinned Stalwart build."""

    def __init__(self, url: str, username: str, password: str) -> None:
        self.url = url
        token = base64.b64encode(f"{username}:{password}".encode()).decode()
        self.headers = {"Authorization": f"Basic {token}", "Content-Type": "application/json"}

    def call(self, object_type: str, operation: str, arguments: dict) -> dict:
        body = json.dumps(
            {
                "using": ["urn:ietf:params:jmap:core", CAPABILITY],
                "methodCalls": [[f"x:{object_type}/{operation}", arguments, "c0"]],
            }
        ).encode()
        request = urllib.request.Request(self.url, data=body, headers=self.headers)
        with urllib.request.urlopen(
            request, timeout=20, context=ssl.create_default_context()
        ) as response:
            payload = json.load(response)
        method, result, _call_id = payload["methodResponses"][0]
        if method == "error" or "notCreated" in result or "notUpdated" in result:
            raise RuntimeError(json.dumps(payload, separators=(",", ":")))
        return result

    def all(self, object_type: str) -> list:
        return self.call(object_type, "get", {})["list"]

    def create(self, object_type: str, key: str, value: dict) -> dict:
        return self.call(object_type, "set", {"create": {key: value}})["created"][key]

    def update(self, object_type: str, object_id: str, value: dict) -> None:
        self.call(object_type, "set", {"update": {object_id: value}})


def by_name(objects: list, name: str):
    return next((item for item in objects if item.get("name") == name), None)


def bound_ports(listener: dict) -> set:
    """Ports a NetworkListener currently binds (keys are literal socket addresses)."""
    ports = set()
    for address in listener.get("bind") or {}:
        host, _, port = address.rpartition(":")
        if host and port.isdigit():
            ports.add(int(port))
    return ports


def ensure_domain(registry: Registry, domain: str) -> str:
    existing = by_name(registry.all("Domain"), domain)
    if existing is None:
        existing = registry.create("Domain", domain, {"name": domain, "isEnabled": True})
    return str(existing["id"])


def ensure_user(registry: Registry, domain_id: str, username: str, password: str) -> None:
    user = by_name(registry.all("Account"), username)
    if user is None:
        registry.create(
            "Account",
            username,
            {
                "@type": "User",
                "name": username,
                "domainId": domain_id,
                "credentials": {"0": {"@type": "Password", "secret": password}},
                "roles": {"@type": "User"},
                "permissions": {"@type": "Inherit"},
            },
        )


def ensure_certificate(registry: Registry) -> None:
    """Install the staged certificate pair as EnvironmentVariable references."""
    desired = {
        "certificate": {"@type": "EnvironmentVariable", "variableName": CERT_ENV},
        "privateKey": {"@type": "EnvironmentVariable", "variableName": KEY_ENV},
    }
    existing = registry.all("Certificate")
    if len(existing) > 1:
        print(
            f"warning: {len(existing)} Certificate objects exist; the TLS config "
            "picks one by SNI. Remove any bootstrap or superseded certificate that "
            "also covers the staging name.",
            file=sys.stderr,
        )
    if existing:
        registry.update("Certificate", str(existing[0]["id"]), desired)
    else:
        registry.create("Certificate", "ato-mail-staging", desired)


def ensure_listeners(registry: Registry, trusted_networks: list) -> list:
    """Repurpose the bootstrap listeners by port, or create the missing ones.

    The pinned build's bootstrap already binds some of these ports (25/465/993/
    8080). A second listener cannot bind the same socket, so this matches by
    PORT and rewrites the found object instead of adding a duplicate.
    """
    existing = registry.all("NetworkListener")
    summary = []
    for name, port, protocol, use_tls, tls_implicit, trust_proxy in LISTENERS:
        desired = {
            "name": name,
            "bind": {f"0.0.0.0:{port}": True},
            "protocol": protocol,
            "useTls": use_tls,
            "tlsImplicit": tls_implicit,
        }
        if trust_proxy:
            desired["overrideProxyTrustedNetworks"] = {cidr: True for cidr in trusted_networks}
        matched = next(
            (listener for listener in existing if port in bound_ports(listener)),
            None,
        )
        if matched is None:
            registry.create("NetworkListener", name, desired)
            summary.append(
                f"created  {port:>5}/{protocol} implicit={tls_implicit} proxy={trust_proxy}"
            )
        else:
            registry.update("NetworkListener", str(matched["id"]), desired)
            summary.append(
                f"updated  {port:>5}/{protocol} implicit={tls_implicit} proxy={trust_proxy} "
                f"(was {matched.get('name')!r})"
            )
    return summary


def ensure_relay(registry: Registry) -> None:
    """Point outbound SMTP at the transport Adapter's internal Port."""
    route = by_name(registry.all("MtaRoute"), "ato-fixed-relay")
    desired = {
        "@type": "Relay",
        "name": "ato-fixed-relay",
        "description": "Ato Stage 1 fixed-target TLS transport Adapter",
        "address": "relay",
        "port": 2525,
        "protocol": "smtp",
        "implicitTls": False,
        "allowInvalidCerts": False,
        "authUsername": required_env("RELAY_AUTH_USERNAME"),
        "authSecret": {"@type": "EnvironmentVariable", "variableName": RELAY_SECRET_ENV},
    }
    if route is None:
        registry.create("MtaRoute", "ato-fixed-relay", desired)
    else:
        # The route name is immutable on this object.
        mutable = {key: value for key, value in desired.items() if key != "name"}
        registry.update("MtaRoute", str(route["id"]), mutable)

    strategy = {
        "route": {
            "match": {"0": {"if": "is_local_domain(rcpt_domain)", "then": "'local'"}},
            "else": "'ato-fixed-relay'",
        },
    }
    strategies = registry.all("MtaOutboundStrategy")
    if strategies:
        registry.update("MtaOutboundStrategy", str(strategies[0]["id"]), strategy)
    else:
        registry.create("MtaOutboundStrategy", "singleton", strategy)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", default="http://127.0.0.1:8080/jmap/")
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="print the plan and exit (no registry call)",
    )
    args = parser.parse_args()

    domain = os.environ.get("STALWART_TEST_DOMAIN", "mx-test.atousercontent.com")
    username = os.environ.get("STALWART_TEST_USER", "ato-mail-test")
    trusted = [
        cidr.strip()
        for cidr in os.environ.get(
            "STALWART_TRUSTED_PROXY_NETWORKS", DEFAULT_TRUSTED_PROXY_NETWORKS
        ).split(",")
        if cidr.strip()
    ]
    if args.dry_run:
        for name, port, protocol, use_tls, tls_implicit, trust_proxy in LISTENERS:
            print(
                f"plan: {name} 0.0.0.0:{port} protocol={protocol} "
                f"useTls={use_tls} implicit={tls_implicit} "
                f"proxyTrusted={trusted if trust_proxy else []}"
            )
        print(
            f"plan: domain {domain!r}, one test account {username!r}, certificate from "
            f"{CERT_ENV}/{KEY_ENV}, relay route to relay:2525"
        )
        return

    registry = Registry(
        args.url,
        required_env("STALWART_ADMIN_USER"),
        required_env("STALWART_ADMIN_PASSWORD"),
    )
    domain_id = ensure_domain(registry, domain)
    ensure_user(registry, domain_id, username, required_env("STALWART_USER_PASSWORD"))
    ensure_certificate(registry)
    for line in ensure_listeners(registry, trusted):
        print(line)
    ensure_relay(registry)
    print(f"staging registry configuration ready for {domain!r} (listeners above)")


if __name__ == "__main__":
    main()
