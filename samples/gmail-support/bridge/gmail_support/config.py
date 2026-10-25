from __future__ import annotations

import json
import os
from dataclasses import dataclass
from pathlib import Path

from .domain import ChatwootTarget, ImportLimits


@dataclass(frozen=True)
class RefreshableBinding:
    value: str | None
    update_url: str
    update_token: str


@dataclass(frozen=True)
class Config:
    database_path: Path
    listen_host: str
    listen_port: int
    app_origin: str
    chatwoot_url: str
    chatwoot_api_token: str
    chatwoot_public_hmac_token: str
    chatwoot_webhook_secret: str
    bridge_admin_token: str
    target: ChatwootTarget
    allowed_agent_ids: frozenset[int]
    mailbox_id: str
    support_address: str
    limits: ImportLimits
    gmail_oauth_json: str | None
    sync_interval_seconds: int
    maximum_message_bytes: int
    egress_socks_url: str | None
    restricted_proxy_target: str | None
    restricted_proxy_authorization: str | None
    google_oauth_client: dict[str, str]
    refreshable_gmail_binding: RefreshableBinding
    ato_account_id: str
    instance_id: str
    oauth_operator_user_id: str
    oauth_state_key: bytes

    @classmethod
    def from_environment(cls) -> "Config":
        runtime = json.loads(_required("ATO_BINDING_CHATWOOT_RUNTIME"))
        gmail_binding = _refreshable_binding(_required("ATO_BINDING_GMAIL_OAUTH"))
        oauth_client = json.loads(_required("ATO_BINDING_GOOGLE_OAUTH_CLIENT"))
        label_id = os.environ.get("GMAIL_SUPPORT_LABEL_ID") or None
        return cls(
            database_path=Path(os.environ.get("BRIDGE_DATABASE", "/var/lib/gmail-bridge/bridge.sqlite3")),
            listen_host=os.environ.get("LISTEN_HOST", "0.0.0.0"),
            listen_port=int(os.environ.get("LISTEN_PORT", "8080")),
            app_origin=_required("ATO_BINDING_APP_ORIGIN").rstrip("/"),
            chatwoot_url=os.environ.get("CHATWOOT_URL", "http://chatwoot:3000"),
            chatwoot_api_token=str(runtime["api_token"]),
            chatwoot_public_hmac_token=str(runtime["inbox_hmac_token"]),
            chatwoot_webhook_secret=str(runtime["webhook_secret"]),
            bridge_admin_token=str(runtime["bridge_admin_token"]),
            target=ChatwootTarget(
                account_id=int(runtime["account_id"]),
                inbox_id=int(runtime["inbox_id"]),
                inbox_identifier=str(runtime["inbox_identifier"]),
            ),
            allowed_agent_ids=frozenset(int(value) for value in runtime["allowed_agent_ids"]),
            mailbox_id=os.environ.get("GMAIL_MAILBOX_ID", "support-primary"),
            support_address=os.environ.get("GMAIL_SUPPORT_ADDRESS", "support@ato.run").lower(),
            limits=ImportLimits(
                query=os.environ.get(
                    "GMAIL_INITIAL_QUERY", "newer_than:30d (to:support@ato.run OR deliveredto:support@ato.run)"
                ),
                label_id=label_id,
                max_messages=int(os.environ.get("GMAIL_INITIAL_MAX_MESSAGES", "100")),
                attachment_max_bytes=int(
                    os.environ.get("GMAIL_ATTACHMENT_MAX_BYTES", str(10 * 1024 * 1024))
                ),
            ),
            gmail_oauth_json=gmail_binding.value,
            sync_interval_seconds=int(os.environ.get("GMAIL_SYNC_INTERVAL_SECONDS", "60")),
            maximum_message_bytes=int(
                os.environ.get("GMAIL_MAXIMUM_MESSAGE_BYTES", str(25 * 1024 * 1024))
            ),
            egress_socks_url=os.environ.get("ATO_BINDING_GOOGLE_HTTPS_EGRESS"),
            restricted_proxy_target=(
                str(runtime["https_proxy_target"])
                if runtime.get("https_proxy_target")
                else None
            ),
            restricted_proxy_authorization=(
                str(runtime["https_proxy_authorization"])
                if runtime.get("https_proxy_authorization")
                else None
            ),
            google_oauth_client={key: str(value) for key, value in oauth_client.items()},
            refreshable_gmail_binding=gmail_binding,
            ato_account_id=str(runtime["ato_account_id"]),
            instance_id=str(runtime["instance_id"]),
            oauth_operator_user_id=str(runtime["oauth_operator_user_id"]),
            oauth_state_key=_state_key(str(runtime["oauth_state_key"])),
        )


def _required(name: str) -> str:
    value = os.environ.get(name)
    if not value:
        raise RuntimeError(f"required binding {name} is absent")
    return value


def _refreshable_binding(value: str) -> RefreshableBinding:
    parsed = json.loads(value)
    if parsed.get("version") != 1 or not isinstance(parsed.get("update"), dict):
        raise RuntimeError("refreshable Gmail Binding envelope is invalid")
    update = parsed["update"]
    if not isinstance(update.get("url"), str) or not isinstance(update.get("token"), str):
        raise RuntimeError("refreshable Gmail Binding capability is invalid")
    current = parsed.get("value")
    if current is not None and not isinstance(current, str):
        raise RuntimeError("refreshable Gmail Binding value is invalid")
    return RefreshableBinding(current, update["url"], update["token"])


def _state_key(value: str) -> bytes:
    import base64

    decoded = base64.urlsafe_b64decode(value + "=" * (-len(value) % 4))
    if len(decoded) != 32:
        raise RuntimeError("oauth_state_key must contain exactly 32 bytes")
    return decoded
