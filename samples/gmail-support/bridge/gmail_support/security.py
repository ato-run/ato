from __future__ import annotations

import hashlib
import hmac
import time
from dataclasses import dataclass
from html import escape
from html.parser import HTMLParser
from urllib.parse import urljoin, urlsplit, urlunsplit


class WebhookRejected(ValueError):
    pass


def verify_chatwoot_signature(
    *,
    raw_body: bytes,
    timestamp: str | None,
    signature: str | None,
    secret: str,
    now: int | None = None,
    tolerance_seconds: int = 300,
) -> None:
    if not timestamp or not signature or not signature.startswith("sha256="):
        raise WebhookRejected("missing Chatwoot signature headers")
    try:
        signed_at = int(timestamp)
    except ValueError as exc:
        raise WebhookRejected("invalid Chatwoot timestamp") from exc
    current = int(time.time()) if now is None else now
    if abs(current - signed_at) > tolerance_seconds:
        raise WebhookRejected("stale Chatwoot webhook")
    expected = hmac.new(
        secret.encode("utf-8"),
        timestamp.encode("ascii") + b"." + raw_body,
        hashlib.sha256,
    ).hexdigest()
    if not hmac.compare_digest(signature, f"sha256={expected}"):
        raise WebhookRejected("invalid Chatwoot signature")


class _SafeHtmlParser(HTMLParser):
    SAFE_TAGS = {
        "a",
        "b",
        "blockquote",
        "br",
        "code",
        "div",
        "em",
        "i",
        "li",
        "ol",
        "p",
        "pre",
        "span",
        "strong",
        "table",
        "tbody",
        "td",
        "th",
        "thead",
        "tr",
        "u",
        "ul",
    }
    VOID_TAGS = {"br"}
    BLOCKED_CONTENT_TAGS = {"iframe", "math", "object", "script", "style", "svg"}

    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.parts: list[str] = []
        self.blocked_depth = 0

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        if tag in self.BLOCKED_CONTENT_TAGS:
            self.blocked_depth += 1
            return
        if self.blocked_depth:
            return
        if tag not in self.SAFE_TAGS:
            return
        safe_attrs: list[str] = []
        if tag == "a":
            for name, value in attrs:
                if name != "href" or not value:
                    continue
                parsed = urlsplit(value)
                if parsed.scheme in {"http", "https", "mailto"}:
                    safe_attrs.append(f'href="{escape(value, quote=True)}"')
                    safe_attrs.append('rel="noreferrer noopener"')
        suffix = " " + " ".join(safe_attrs) if safe_attrs else ""
        self.parts.append(f"<{tag}{suffix}>")

    def handle_endtag(self, tag: str) -> None:
        if tag in self.BLOCKED_CONTENT_TAGS and self.blocked_depth:
            self.blocked_depth -= 1
            return
        if self.blocked_depth:
            return
        if tag in self.SAFE_TAGS and tag not in self.VOID_TAGS:
            self.parts.append(f"</{tag}>")

    def handle_data(self, data: str) -> None:
        if not self.blocked_depth:
            self.parts.append(escape(data))


def sanitize_email_html(value: str) -> str:
    """Keep a small safe subset and drop images, scripts, styles, and tracking URLs."""
    parser = _SafeHtmlParser()
    parser.feed(value)
    parser.close()
    return "".join(parser.parts)


@dataclass(frozen=True)
class FixedConnectTarget:
    host: str
    port: int


def validate_connect_target(authority: str, allowed_hosts: frozenset[str]) -> FixedConnectTarget:
    if authority.count(":") != 1:
        raise ValueError("CONNECT target must be host:port")
    host, raw_port = authority.rsplit(":", 1)
    host = host.rstrip(".").lower()
    try:
        port = int(raw_port)
    except ValueError as exc:
        raise ValueError("CONNECT port is invalid") from exc
    if host not in allowed_hosts or port != 443:
        raise ValueError("CONNECT target is not allowlisted")
    return FixedConnectTarget(host=host, port=port)


def internal_chatwoot_attachment_url(
    value: str, *, internal_base_url: str, public_base_url: str
) -> str:
    """Map only the configured public Chatwoot origin back to its loopback origin."""
    internal = urlsplit(internal_base_url)
    public = urlsplit(public_base_url)
    candidate = urlsplit(urljoin(internal_base_url.rstrip("/") + "/", value))
    if candidate.username or candidate.password:
        raise ValueError("Chatwoot attachment URL must not contain credentials")

    def origin(parts) -> tuple[str, str | None, int | None]:
        try:
            port = parts.port
        except ValueError as exc:
            raise ValueError("Chatwoot attachment URL has an invalid port") from exc
        if port is None:
            port = 443 if parts.scheme.lower() == "https" else 80
        return parts.scheme.lower(), parts.hostname, port

    if origin(candidate) == origin(public):
        return urlunsplit(
            (internal.scheme, internal.netloc, candidate.path, candidate.query, "")
        )
    if origin(candidate) != origin(internal):
        raise ValueError("Chatwoot attachment URL has an unexpected origin")
    return urlunsplit(
        (internal.scheme, internal.netloc, candidate.path, candidate.query, "")
    )
