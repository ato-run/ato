#!/usr/bin/env python3
"""Write and verify one uniquely identifiable draft in a dedicated test mailbox."""

import argparse
from email import policy
from email.message import EmailMessage
from email.parser import BytesParser
import getpass
import hashlib
import imaplib
import json
from pathlib import Path
import secrets
import ssl
import sys
import uuid


def require_ok(result, operation):
    status, data = result
    if status != "OK":
        raise ValueError(f"IMAP {operation} failed ({status})")
    return data


def marker():
    message = EmailMessage(policy=policy.SMTP)
    message["Message-ID"] = f"<{uuid.uuid4()}@selfhost-test.invalid>"
    message["Subject"] = "Ato self-host restart check"
    message.set_content(secrets.token_hex(32))
    return message


def body_hash(message):
    payload = message.get_payload(decode=True)
    if not isinstance(payload, bytes):
        raise ValueError("Expected a plain draft")
    return hashlib.sha256(payload.rstrip(b"\r\n")).hexdigest()


def verify(client, evidence):
    mid = evidence["message_id"]
    if mid != f"<{uuid.UUID(mid[1:37])}@selfhost-test.invalid>":
        raise ValueError("Invalid evidence Message-ID")
    matches = require_ok(client.uid("search", None, "HEADER", "Message-ID", mid), "SEARCH")
    uids = matches[0].split()
    if len(uids) != 1 or not uids[0].isdigit():
        raise ValueError("Expected exactly one matching draft")
    data = require_ok(client.uid("fetch", uids[0], "(BODY.PEEK[]<0.16384>)"), "FETCH")
    literals = [entry[1] for entry in data if isinstance(entry, tuple)]
    if len(literals) != 1 or len(literals[0]) >= 16384:
        raise ValueError("Expected one complete test message")
    message = BytesParser(policy=policy.default).parsebytes(literals[0])
    if message["Message-ID"] != mid or body_hash(message) != evidence["body_sha256"]:
        raise ValueError("Draft identity or body differs from the saved evidence")


def run(args):
    if not sys.stdin.isatty() and not args.password_stdin:
        raise ValueError("Supply the test mailbox password on stdin using --password-stdin")
    password = (sys.stdin.readline().rstrip("\r\n") if args.password_stdin
                else getpass.getpass("Test mailbox password: "))
    if not password:
        raise ValueError("Empty password")
    context = ssl.create_default_context(cafile=args.ca_file)
    endpoint = {"host": args.host, "port": args.port, "account": args.account}
    with imaplib.IMAP4_SSL(args.host, args.port, ssl_context=context, timeout=20) as client:
        require_ok(client.login(args.account, password), "LOGIN")
        require_ok(client.select("INBOX", readonly=args.action == "verify"), "SELECT")
        if args.action == "write":
            message = marker()
            evidence = {**endpoint, "message_id": str(message["Message-ID"]),
                        "body_sha256": body_hash(message)}
            # Preserve the identity for a read-only retry if APPEND loses its reply.
            with args.evidence.open("x", encoding="utf-8") as stream:
                json.dump(evidence, stream)
                stream.write("\n")
            require_ok(client.append("INBOX", r"(\Draft)", None, message.as_bytes()), "APPEND")
        else:
            evidence = json.loads(args.evidence.read_text(encoding="utf-8"))
            if any(evidence.get(key) != value for key, value in endpoint.items()):
                raise ValueError("Use the same host, port and test account")
        verify(client, evidence)
    print("Test draft and body verified over IMAPS")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("write", "verify"))
    parser.add_argument("--host", required=True)
    parser.add_argument("--port", type=int, default=993)
    parser.add_argument("--account", required=True, help="Dedicated test account")
    parser.add_argument("--evidence", type=Path, required=True)
    parser.add_argument("--ca-file", help="PEM trust root for an internal CA")
    parser.add_argument("--password-stdin", action="store_true")
    try:
        run(parser.parse_args())
    except (OSError, ValueError, KeyError, imaplib.IMAP4.error) as exc:
        sys.exit(str(exc))


if __name__ == "__main__":
    main()
