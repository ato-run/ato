#!/usr/bin/env python3
"""Restricted implicit-TLS SMTP relay used by the staging acceptance.

This is deliberately a sink, not a general relay. It requires AUTH, accepts
only a single configured recipient domain, and writes one JSON record per
accepted message. It has no dependency outside the Python standard library.
"""

from __future__ import annotations

import argparse
import asyncio
import base64
import hashlib
import hmac
import json
import os
import ssl
from datetime import UTC, datetime
from pathlib import Path


MAX_COMMAND_BYTES = 8 * 1024
MAX_MESSAGE_BYTES = 10 * 1024 * 1024


def required_env(name: str) -> str:
    value = os.environ.get(name, "")
    if not value:
        raise SystemExit(f"{name} is required")
    return value


def decode_plain(value: str) -> tuple[str, str] | None:
    try:
        decoded = base64.b64decode(value, validate=True).decode("utf-8")
        _authorization, username, password = decoded.split("\0", 2)
        return username, password
    except (ValueError, UnicodeDecodeError):
        return None


def address_from_command(command: str) -> str | None:
    left = command.find("<")
    right = command.find(">", left + 1)
    if left < 0 or right < 0:
        return None
    return command[left + 1 : right].strip().lower()


class Session:
    def __init__(self, writer: asyncio.StreamWriter, allowed_domain: str) -> None:
        self.writer = writer
        self.allowed_domain = allowed_domain
        self.authenticated = False
        self.sender: str | None = None
        self.recipients: list[str] = []

    async def reply(self, code: int, message: str) -> None:
        self.writer.write(f"{code} {message}\r\n".encode())
        await self.writer.drain()

    def reset_envelope(self) -> None:
        self.sender = None
        self.recipients.clear()


class RelaySink:
    def __init__(self, output: Path, username: str, password: str, allowed_domain: str) -> None:
        self.output = output
        self.username = username
        self.password = password
        self.allowed_domain = allowed_domain.lower()
        output.parent.mkdir(parents=True, exist_ok=True)

    def credentials_match(self, username: str, password: str) -> bool:
        return hmac.compare_digest(username, self.username) and hmac.compare_digest(
            password, self.password
        )

    async def handle(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        peer = writer.get_extra_info("peername")
        session = Session(writer, self.allowed_domain)
        await session.reply(220, "relay.ato-mail.test ESMTP staging sink")
        try:
            while line := await reader.readline():
                if len(line) > MAX_COMMAND_BYTES:
                    await session.reply(500, "command too long")
                    break
                command = line.decode("utf-8", "replace").rstrip("\r\n")
                verb, _, argument = command.partition(" ")
                verb = verb.upper()
                if verb in {"EHLO", "HELO"}:
                    writer.write(
                        b"250-relay.ato-mail.test\r\n"
                        b"250-AUTH PLAIN\r\n"
                        b"250-SIZE 10485760\r\n"
                        b"250 8BITMIME\r\n"
                    )
                    await writer.drain()
                elif verb == "AUTH":
                    mechanism, _, payload = argument.partition(" ")
                    decoded = decode_plain(payload) if mechanism.upper() == "PLAIN" else None
                    if decoded and self.credentials_match(*decoded):
                        session.authenticated = True
                        await session.reply(235, "authentication successful")
                    else:
                        await session.reply(535, "authentication failed")
                elif verb == "MAIL":
                    if not session.authenticated:
                        await session.reply(530, "authentication required")
                        continue
                    address = address_from_command(argument)
                    if address is None:
                        await session.reply(501, "invalid sender")
                        continue
                    session.reset_envelope()
                    session.sender = address
                    await session.reply(250, "sender accepted")
                elif verb == "RCPT":
                    if not session.authenticated:
                        await session.reply(530, "authentication required")
                        continue
                    address = address_from_command(argument)
                    if not session.sender or address is None:
                        await session.reply(503, "MAIL required")
                    elif not address.endswith("@" + self.allowed_domain):
                        await session.reply(550, "recipient domain not permitted")
                    else:
                        session.recipients.append(address)
                        await session.reply(250, "recipient accepted")
                elif verb == "DATA":
                    if not session.sender or not session.recipients:
                        await session.reply(503, "MAIL and RCPT required")
                        continue
                    await session.reply(354, "end with <CRLF>.<CRLF>")
                    message = await self.read_message(reader)
                    if message is None:
                        await session.reply(552, "message too large")
                        session.reset_envelope()
                        continue
                    self.record(peer, session.sender, session.recipients, message)
                    await session.reply(250, "message accepted for staging receipt")
                    session.reset_envelope()
                elif verb == "RSET":
                    session.reset_envelope()
                    await session.reply(250, "reset")
                elif verb == "NOOP":
                    await session.reply(250, "ok")
                elif verb == "QUIT":
                    await session.reply(221, "bye")
                    break
                else:
                    await session.reply(502, "command not implemented")
        except (ConnectionError, asyncio.IncompleteReadError):
            pass
        finally:
            writer.close()
            await writer.wait_closed()

    @staticmethod
    async def read_message(reader: asyncio.StreamReader) -> bytes | None:
        chunks: list[bytes] = []
        total = 0
        oversize = False
        while line := await reader.readline():
            if line in {b".\r\n", b".\n"}:
                break
            if line.startswith(b".."):
                line = line[1:]
            total += len(line)
            if total > MAX_MESSAGE_BYTES:
                oversize = True
            elif not oversize:
                chunks.append(line)
        return None if oversize else b"".join(chunks)

    def record(self, peer: object, sender: str, recipients: list[str], message: bytes) -> None:
        record = {
            "received_at": datetime.now(UTC).isoformat(),
            "peer": str(peer),
            "sender": sender,
            "recipients": recipients,
            "message_sha256": hashlib.sha256(message).hexdigest(),
            "message_base64": base64.b64encode(message).decode("ascii"),
        }
        with self.output.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(record, separators=(",", ":")) + "\n")


async def run(args: argparse.Namespace) -> None:
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    context.load_cert_chain(args.certificate, args.private_key)
    sink = RelaySink(
        Path(args.output),
        required_env("RELAY_AUTH_USERNAME"),
        required_env("RELAY_AUTH_PASSWORD"),
        args.allowed_domain,
    )
    server = await asyncio.start_server(sink.handle, args.host, args.port, ssl=context)
    async with server:
        await server.serve_forever()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="0.0.0.0")
    parser.add_argument("--port", type=int, default=2465)
    parser.add_argument("--certificate", required=True)
    parser.add_argument("--private-key", required=True)
    parser.add_argument("--allowed-domain", default="relay.test")
    parser.add_argument("--output", required=True)
    asyncio.run(run(parser.parse_args()))


if __name__ == "__main__":
    main()
