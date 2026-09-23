from __future__ import annotations

import hashlib
import hmac
from email.utils import parseaddr
from typing import Any

import aiohttp

from .domain import ChatwootTarget, Direction, MailMessage, OutgoingAttachment
from .security import internal_chatwoot_attachment_url


class ChatwootClient:
    def __init__(
        self,
        *,
        base_url: str,
        api_token: str,
        target: ChatwootTarget,
        public_hmac_token: str,
        public_base_url: str | None = None,
        timeout_seconds: int = 20,
    ) -> None:
        self.base_url = base_url.rstrip("/")
        self.public_base_url = public_base_url.rstrip("/") if public_base_url else None
        self.api_token = api_token
        self.target = target
        self.public_hmac_token = public_hmac_token
        self.timeout = aiohttp.ClientTimeout(total=timeout_seconds)

    async def import_message(
        self, *, message: MailMessage, existing_conversation_id: int | None
    ) -> int:
        reply_target = message.reply_to or parseaddr(message.sender)[1]
        gmail_attributes = {
            "ato_mailbox_id": message.mailbox_id,
            "gmail_thread_id": message.gmail_thread_id,
            "gmail_subject": message.subject,
            "gmail_reply_to": reply_target,
            "gmail_recipients": list(message.recipients),
        }
        if existing_conversation_id is None:
            source_id = self._source_id(message.sender)
            contact = await self._post(
                f"/public/api/v1/inboxes/{self.target.inbox_identifier}/contacts",
                {
                    "source_id": source_id,
                    "identifier": source_id,
                    "identifier_hash": self._identifier_hash(source_id),
                    "email": parseaddr(message.sender)[1],
                    "name": parseaddr(message.sender)[0] or parseaddr(message.sender)[1],
                },
                authenticated=False,
            )
            contact_id = int(contact["id"])
            existing_conversation_id = await self._find_imported_conversation(
                contact_id=contact_id,
                mailbox_id=message.mailbox_id,
                gmail_thread_id=message.gmail_thread_id,
            )
            if existing_conversation_id is None:
                contact_source_id = str(
                    contact.get("source_id") or contact.get("id") or source_id
                )
                conversation = await self._post(
                    (
                        f"/public/api/v1/inboxes/{self.target.inbox_identifier}/contacts/"
                        f"{contact_source_id}/conversations"
                    ),
                    {"custom_attributes": gmail_attributes},
                    authenticated=False,
                )
                existing_conversation_id = int(conversation["id"])
        elif message.direction == Direction.INCOMING:
            await self._post(
                (
                    f"/api/v1/accounts/{self.target.account_id}/conversations/"
                    f"{existing_conversation_id}/custom_attributes"
                ),
                {"custom_attributes": gmail_attributes, "merge": True},
                authenticated=True,
            )
        if await self._imported_message_exists(
            conversation_id=existing_conversation_id,
            gmail_message_id=message.gmail_message_id,
        ):
            return existing_conversation_id
        message_type = (
            "outgoing" if message.direction == Direction.OUTGOING_IMPORTED else "incoming"
        )
        path = (
            f"/api/v1/accounts/{self.target.account_id}/conversations/"
            f"{existing_conversation_id}/messages"
        )
        fields: dict[str, object] = {
            "content": message.html or message.text,
            "message_type": message_type,
            "private": False,
            "source_id": f"gmail:{message.mailbox_id}:{message.gmail_message_id}",
            "external_created_at": message.sent_at_ms // 1000,
            "content_attributes": {
                "ato_imported_from_gmail": True,
                "gmail_message_id": message.gmail_message_id,
                "gmail_direction": message.direction,
            },
        }
        if message.attachments:
            await self._post_multipart(path, fields, message.attachments)
        else:
            await self._post(path, fields, authenticated=True)
        return existing_conversation_id

    async def _find_imported_conversation(
        self, *, contact_id: int, mailbox_id: str, gmail_thread_id: str
    ) -> int | None:
        result = await self._get(
            (
                f"/api/v1/accounts/{self.target.account_id}/contacts/"
                f"{contact_id}/conversations"
            ),
            authenticated=True,
        )
        conversations = result.get("payload") or []
        for conversation in conversations:
            if not isinstance(conversation, dict):
                continue
            attributes = conversation.get("custom_attributes") or {}
            if not isinstance(attributes, dict):
                continue
            if (
                attributes.get("ato_mailbox_id") == mailbox_id
                and attributes.get("gmail_thread_id") == gmail_thread_id
            ):
                return int(conversation["id"])
        return None

    async def _imported_message_exists(
        self, *, conversation_id: int, gmail_message_id: str
    ) -> bool:
        result = await self._get(
            (
                f"/api/v1/accounts/{self.target.account_id}/conversations/"
                f"{conversation_id}/messages"
            ),
            authenticated=True,
        )
        messages = result.get("payload") or result.get("messages") or []
        for item in messages:
            if not isinstance(item, dict):
                continue
            attributes = item.get("content_attributes") or {}
            if (
                isinstance(attributes, dict)
                and attributes.get("gmail_message_id") == gmail_message_id
            ):
                return True
        return False

    async def get_message(
        self, *, account_id: int, conversation_id: int, message_id: int
    ) -> dict[str, object]:
        result = await self._get(
            f"/api/v1/accounts/{account_id}/conversations/{conversation_id}/messages",
            authenticated=True,
        )
        values = result.get("payload") or result.get("messages") or []
        for value in values:
            if isinstance(value, dict) and int(value.get("id", -1)) == message_id:
                return value
        raise RuntimeError("Chatwoot message was not found in its conversation")

    async def get_conversation(
        self, *, account_id: int, conversation_id: int
    ) -> dict[str, object]:
        return await self._get(
            f"/api/v1/accounts/{account_id}/conversations/{conversation_id}",
            authenticated=True,
        )

    async def update_message_status(
        self,
        *,
        account_id: int,
        conversation_id: int,
        message_id: int,
        status: str,
        external_error: str | None = None,
    ) -> None:
        body: dict[str, object] = {"status": status}
        if external_error:
            body["external_error"] = external_error
        await self._request(
            "PATCH",
            (
                f"/api/v1/accounts/{account_id}/conversations/{conversation_id}/"
                f"messages/{message_id}"
            ),
            body,
            authenticated=True,
        )

    async def download_reply_attachments(
        self, *, attachments: list[dict[str, object]], maximum_total_bytes: int
    ) -> tuple[OutgoingAttachment, ...]:
        downloaded: list[OutgoingAttachment] = []
        total = 0
        headers = {"api_access_token": self.api_token}
        async with aiohttp.ClientSession(timeout=self.timeout, headers=headers) as session:
            for item in attachments:
                url_value = item.get("data_url") or item.get("file_url")
                if not isinstance(url_value, str):
                    raise ValueError("Chatwoot attachment URL is missing")
                url = internal_chatwoot_attachment_url(
                    url_value,
                    internal_base_url=self.base_url,
                    public_base_url=self.public_base_url or self.base_url,
                )
                async with session.get(url) as response:
                    if response.status != 200:
                        raise RuntimeError("Chatwoot attachment download failed")
                    chunks: list[bytes] = []
                    async for chunk in response.content.iter_chunked(64 * 1024):
                        total += len(chunk)
                        if total > maximum_total_bytes:
                            raise ValueError("reply attachments exceed the configured size limit")
                        chunks.append(chunk)
                    filename = str(item.get("file_name") or item.get("filename") or "attachment")
                    content_type = str(
                        item.get("file_type")
                        or response.headers.get("Content-Type")
                        or "application/octet-stream"
                    )
                    downloaded.append(
                        OutgoingAttachment(filename, content_type, b"".join(chunks))
                    )
        return tuple(downloaded)

    def _source_id(self, sender: str) -> str:
        address = parseaddr(sender)[1].lower()
        return "gmail-" + hashlib.sha256(address.encode()).hexdigest()[:32]

    def _identifier_hash(self, identifier: str) -> str:
        return hmac.new(
            self.public_hmac_token.encode(), identifier.encode(), hashlib.sha256
        ).hexdigest()

    async def _get(self, path: str, *, authenticated: bool) -> dict[str, Any]:
        return await self._request("GET", path, None, authenticated=authenticated)

    async def _post(
        self, path: str, body: dict[str, object], *, authenticated: bool
    ) -> dict[str, Any]:
        return await self._request("POST", path, body, authenticated=authenticated)

    async def _post_multipart(
        self, path: str, fields: dict[str, object], attachments: tuple
    ) -> dict[str, Any]:
        form = aiohttp.FormData()
        for key, value in fields.items():
            if isinstance(value, dict):
                import json

                form.add_field(key, json.dumps(value, separators=(",", ":")))
            elif isinstance(value, bool):
                form.add_field(key, "true" if value else "false")
            else:
                form.add_field(key, str(value))
        for attachment in attachments:
            form.add_field(
                "attachments[]",
                attachment.content,
                filename=attachment.filename,
                content_type=attachment.content_type,
            )
        headers = {"api_access_token": self.api_token}
        async with aiohttp.ClientSession(timeout=self.timeout, headers=headers) as session:
            async with session.post(self.base_url + path, data=form) as response:
                payload = await response.json(content_type=None)
                if response.status >= 400:
                    raise RuntimeError(f"Chatwoot API rejected attachment import: {response.status}")
                return payload

    async def _request(
        self,
        method: str,
        path: str,
        body: dict[str, object] | None,
        *,
        authenticated: bool,
    ) -> dict[str, Any]:
        headers = {"Accept": "application/json"}
        if authenticated:
            headers["api_access_token"] = self.api_token
        async with aiohttp.ClientSession(timeout=self.timeout) as session:
            async with session.request(
                method, self.base_url + path, json=body, headers=headers
            ) as response:
                payload = await response.json(content_type=None)
                if response.status >= 400:
                    raise RuntimeError(f"Chatwoot API rejected {method} {path}: {response.status}")
                if not isinstance(payload, dict):
                    raise RuntimeError("Chatwoot API returned a non-object response")
                return payload
