#!/usr/bin/env python3
"""Verify one message through a fixed-address implicit-TLS IMAP endpoint.

The password is read as one line from stdin. The TCP destination and verified
TLS identity are separate so this exercises the same fixed-address boundary as
the staging acceptance rather than relying on DNS to find the Runner.
"""

from __future__ import annotations

import argparse
import imaplib
import json
import socket
import ssl
import sys
from email import policy
from email.parser import BytesParser


class FixedAddressIMAP4SSL(imaplib.IMAP4_SSL):
    def __init__(
        self,
        connect_host: str,
        port: int,
        tls_server_name: str,
        context: ssl.SSLContext,
    ) -> None:
        self._tls_server_name = tls_server_name
        super().__init__(connect_host, port, ssl_context=context, timeout=10)

    def _create_socket(self, timeout: float | None) -> socket.socket:
        raw_socket = socket.create_connection((self.host, self.port), timeout)
        return self.ssl_context.wrap_socket(
            raw_socket,
            server_hostname=self._tls_server_name,
        )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--connect-host", required=True)
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--tls-server-name", required=True)
    parser.add_argument("--ca-file", required=True)
    parser.add_argument("--username", required=True)
    parser.add_argument("--subject", required=True)
    args = parser.parse_args()

    password = sys.stdin.readline().rstrip("\n")
    if not password:
        raise SystemExit("IMAP password was not provided on stdin")

    context = ssl.create_default_context(cafile=args.ca_file)
    client = FixedAddressIMAP4SSL(
        args.connect_host,
        args.port,
        args.tls_server_name,
        context,
    )
    try:
        tls_socket = client.sock
        certificate = tls_socket.getpeercert()
        client.login(args.username, password)
        status, _mailbox = client.select("INBOX", readonly=True)
        if status != "OK":
            raise RuntimeError(f"INBOX select failed: {status}")
        status, message_ids = client.search(None, "SUBJECT", f'"{args.subject}"')
        if status != "OK" or not message_ids or not message_ids[0].split():
            raise RuntimeError("expected subject was not found")
        message_id = message_ids[0].split()[-1]
        status, parts = client.fetch(
            message_id,
            "(BODY.PEEK[HEADER.FIELDS (SUBJECT FROM TO)] RFC822.SIZE)",
        )
        if status != "OK":
            raise RuntimeError(f"message fetch failed: {status}")
        header = next(
            (part[1] for part in parts if isinstance(part, tuple)),
            None,
        )
        if not isinstance(header, bytes):
            raise RuntimeError("message header was missing")
        parsed = BytesParser(policy=policy.default).parsebytes(header)
        print(
            json.dumps(
                {
                    "found": True,
                    "subject": str(parsed["subject"]),
                    "from": str(parsed["from"]),
                    "to": str(parsed["to"]),
                    "tls_version": tls_socket.version(),
                    "subject_alt_names": certificate.get("subjectAltName", []),
                },
                separators=(",", ":"),
            )
        )
    finally:
        try:
            client.logout()
        except (OSError, imaplib.IMAP4.error):
            client.shutdown()


if __name__ == "__main__":
    main()
