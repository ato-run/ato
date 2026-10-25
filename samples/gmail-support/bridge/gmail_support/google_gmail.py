from __future__ import annotations

import asyncio
import base64
import email
import json
from email.header import decode_header, make_header
from email.policy import default
from email.utils import getaddresses
from typing import Any

from google.oauth2.credentials import Credentials
from googleapiclient.discovery import build
from googleapiclient.errors import HttpError

from .domain import (
    Attachment,
    AuthorizationExpired,
    Direction,
    HistoryExpired,
    MailMessage,
    OutcomeUnknown,
    PermanentSendFailure,
    SendReceipt,
)


def _b64url_decode(value: str) -> bytes:
    return base64.urlsafe_b64decode(value + "=" * (-len(value) % 4))


def _decoded_header(message: email.message.EmailMessage, name: str) -> str:
    value = message.get(name, "")
    return str(make_header(decode_header(str(value))))


class GoogleGmail:
    def __init__(
        self,
        *,
        mailbox_id: str,
        credential_json: str,
        maximum_message_bytes: int,
        maximum_history_messages: int = 500,
    ) -> None:
        values = json.loads(credential_json)
        required = {"client_id", "client_secret", "refresh_token"}
        if not required.issubset(values):
            raise ValueError("Gmail credential binding is incomplete")
        self.mailbox_id = mailbox_id
        self.maximum_message_bytes = maximum_message_bytes
        self.maximum_history_messages = maximum_history_messages
        self.credentials = Credentials(
            token=None,
            refresh_token=values["refresh_token"],
            token_uri=values.get("token_uri", "https://oauth2.googleapis.com/token"),
            client_id=values["client_id"],
            client_secret=values["client_secret"],
            scopes=values.get(
                "scopes",
                [
                    "https://www.googleapis.com/auth/gmail.readonly",
                    "https://www.googleapis.com/auth/gmail.send",
                ],
            ),
        )
        self.api = build(
            "gmail",
            "v1",
            credentials=self.credentials,
            cache_discovery=False,
            static_discovery=True,
        )

    async def profile(self) -> dict[str, object]:
        return await self._execute(self.api.users().getProfile(userId="me"))

    async def send_as(self) -> list[dict[str, object]]:
        result = await self._execute(self.api.users().settings().sendAs().list(userId="me"))
        return list(result.get("sendAs", []))

    async def list_message_ids(
        self, *, query: str, label_id: str | None, limit: int
    ) -> tuple[list[str], str]:
        identifiers: list[str] = []
        page_token: str | None = None
        while len(identifiers) < limit:
            request = self.api.users().messages().list(
                userId="me",
                q=query,
                labelIds=[label_id] if label_id else None,
                maxResults=min(500, limit - len(identifiers)),
                pageToken=page_token,
            )
            page = await self._execute(request)
            identifiers.extend(str(item["id"]) for item in page.get("messages", []))
            page_token = page.get("nextPageToken")
            if not page_token:
                break
        profile = await self.profile()
        return identifiers[:limit], str(profile["historyId"])

    async def history_message_ids(
        self, *, start_history_id: str, label_id: str | None
    ) -> tuple[list[str], str]:
        identifiers: list[str] = []
        page_token: str | None = None
        last_history_id = start_history_id
        try:
            while True:
                page = await self._execute(
                    self.api.users().history().list(
                        userId="me",
                        startHistoryId=start_history_id,
                        historyTypes=["messageAdded"],
                        labelId=label_id,
                        maxResults=min(500, self.maximum_history_messages),
                        pageToken=page_token,
                    )
                )
                for event in page.get("history", []):
                    for item in event.get("messagesAdded", []):
                        identifier = str(item["message"]["id"])
                        if identifier not in identifiers:
                            identifiers.append(identifier)
                            if len(identifiers) > self.maximum_history_messages:
                                raise RuntimeError("Gmail history result exceeds the configured bound")
                last_history_id = str(page.get("historyId", last_history_id))
                page_token = page.get("nextPageToken")
                if not page_token:
                    break
        except HttpError as exc:
            if exc.resp.status == 404:
                raise HistoryExpired from exc
            raise
        return identifiers, last_history_id

    async def get_message(self, message_id: str) -> MailMessage:
        metadata = await self._execute(
            self.api.users().messages().get(userId="me", id=message_id, format="minimal")
        )
        if int(metadata.get("sizeEstimate", 0)) > self.maximum_message_bytes:
            raise ValueError("Gmail message exceeds the configured size limit")
        resource = await self._execute(
            self.api.users().messages().get(userId="me", id=message_id, format="raw")
        )
        raw = _b64url_decode(str(resource["raw"]))
        if len(raw) > self.maximum_message_bytes:
            raise ValueError("decoded Gmail message exceeds the configured size limit")
        parsed = email.message_from_bytes(raw, policy=default)
        text_parts: list[str] = []
        html_parts: list[str] = []
        attachments: list[Attachment] = []
        for index, part in enumerate(parsed.walk()):
            if part.is_multipart():
                continue
            content_disposition = part.get_content_disposition()
            filename = part.get_filename()
            payload = part.get_payload(decode=True) or b""
            if content_disposition == "attachment" or filename:
                attachments.append(
                    Attachment(
                        attachment_id=f"raw-{index}",
                        filename=filename or "attachment",
                        content_type=part.get_content_type(),
                        size=len(payload),
                        content=payload,
                    )
                )
                continue
            charset = part.get_content_charset() or "utf-8"
            value = payload.decode(charset, errors="replace")
            if part.get_content_type() == "text/plain":
                text_parts.append(value)
            elif part.get_content_type() == "text/html":
                html_parts.append(value)
        labels = frozenset(str(item) for item in resource.get("labelIds", []))
        headers = {str(key): str(value) for key, value in parsed.items()}
        references = tuple(str(parsed.get("References", "")).split())
        reply_to_values = getaddresses([str(parsed.get("Reply-To", ""))])
        reply_to = reply_to_values[0][1] if reply_to_values else None
        recipients = tuple(address for _, address in getaddresses(parsed.get_all("To", [])))
        return MailMessage(
            mailbox_id=self.mailbox_id,
            gmail_message_id=str(resource["id"]),
            gmail_thread_id=str(resource["threadId"]),
            rfc_message_id=str(parsed.get("Message-ID", "")),
            subject=_decoded_header(parsed, "Subject"),
            sender=_decoded_header(parsed, "From"),
            recipients=recipients,
            reply_to=reply_to,
            references=references,
            sent_at_ms=int(resource.get("internalDate", 0)),
            text="\n".join(text_parts),
            html="\n".join(html_parts) or None,
            label_ids=labels,
            headers=headers,
            attachments=tuple(attachments),
            direction=(
                Direction.OUTGOING_IMPORTED if "SENT" in labels else Direction.INCOMING
            ),
        )

    async def send_raw(
        self, *, raw: str, thread_id: str, request_id: str
    ) -> SendReceipt:
        request = self.api.users().messages().send(
            userId="me", body={"raw": raw, "threadId": thread_id}
        )
        try:
            result = await self._execute(request)
        except HttpError as exc:
            if 400 <= exc.resp.status < 500 and exc.resp.status not in {408, 429}:
                raise PermanentSendFailure(f"Gmail rejected the send request ({exc.resp.status})") from exc
            raise OutcomeUnknown("Gmail send result is unknown after an API error") from exc
        except AuthorizationExpired:
            raise
        except Exception as exc:
            raise OutcomeUnknown("Gmail send result is unknown after a transport failure") from exc
        return SendReceipt(
            gmail_message_id=str(result["id"]),
            gmail_thread_id=str(result["threadId"]),
            history_id=str(result["historyId"]) if result.get("historyId") else None,
        )

    @staticmethod
    async def _execute(request: Any) -> dict[str, Any]:
        try:
            return await asyncio.to_thread(request.execute)
        except HttpError as exc:
            if exc.resp.status in {401, 403}:
                raise AuthorizationExpired("Google authorization is no longer valid") from exc
            raise
