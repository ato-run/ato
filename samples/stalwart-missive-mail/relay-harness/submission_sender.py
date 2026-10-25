#!/usr/bin/env python3
"""Send one acceptance message through implicit-TLS SMTP Submission.

The password is read as one line from stdin so it is never placed in argv or
the workspace. The TCP destination and verified TLS identity are deliberately
separate, matching a fixed Ato TCP allocation addressed by IP.
"""

from __future__ import annotations

import argparse
import json
import smtplib
import socket
import ssl
import sys
from email.message import EmailMessage


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--connect-host", required=True)
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--tls-server-name", required=True)
    parser.add_argument("--ca-file", required=True)
    parser.add_argument("--username", required=True)
    parser.add_argument("--from", dest="sender", required=True)
    parser.add_argument("--to", dest="recipient", required=True)
    parser.add_argument("--subject", required=True)
    parser.add_argument("--body", required=True)
    args = parser.parse_args()

    password = sys.stdin.readline().rstrip("\n")
    if not password:
        raise SystemExit("SMTP password was not provided on stdin")

    context = ssl.create_default_context(cafile=args.ca_file)
    raw_socket = socket.create_connection((args.connect_host, args.port), timeout=10)
    tls_socket = context.wrap_socket(raw_socket, server_hostname=args.tls_server_name)

    smtp = smtplib.SMTP(timeout=10)
    smtp._host = args.tls_server_name  # Used only by smtplib error reporting.
    smtp.sock = tls_socket
    smtp.file = None
    code, greeting = smtp.getreply()
    if code != 220:
        raise RuntimeError(f"unexpected SMTP greeting {code}: {greeting!r}")

    try:
        smtp.ehlo()
        smtp.login(args.username, password)
        message = EmailMessage()
        message["From"] = args.sender
        message["To"] = args.recipient
        message["Subject"] = args.subject
        message.set_content(args.body)
        refused = smtp.send_message(message)
        if refused:
            raise RuntimeError(f"SMTP server refused recipients: {sorted(refused)}")
        print(
            json.dumps(
                {
                    "submitted": True,
                    "recipient": args.recipient,
                    "subject": args.subject,
                    "tls_version": tls_socket.version(),
                },
                separators=(",", ":"),
            )
        )
    finally:
        try:
            smtp.quit()
        except (OSError, smtplib.SMTPException):
            smtp.close()


if __name__ == "__main__":
    main()
