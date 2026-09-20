from __future__ import annotations

from typing import Protocol, Sequence

from .domain import MailMessage, OutgoingAttachment, SendReceipt


class GmailPort(Protocol):
    async def profile(self) -> dict[str, object]: ...

    async def send_as(self) -> Sequence[dict[str, object]]: ...

    async def list_message_ids(
        self, *, query: str, label_id: str | None, limit: int
    ) -> tuple[list[str], str]: ...

    async def history_message_ids(
        self, *, start_history_id: str, label_id: str | None
    ) -> tuple[list[str], str]: ...

    async def get_message(self, message_id: str) -> MailMessage: ...

    async def send_raw(
        self, *, raw: str, thread_id: str, request_id: str
    ) -> SendReceipt: ...


class ChatwootPort(Protocol):
    async def import_message(
        self, *, message: MailMessage, existing_conversation_id: int | None
    ) -> int: ...

    async def get_message(
        self, *, account_id: int, conversation_id: int, message_id: int
    ) -> dict[str, object]: ...

    async def get_conversation(
        self, *, account_id: int, conversation_id: int
    ) -> dict[str, object]: ...

    async def update_message_status(
        self,
        *,
        account_id: int,
        conversation_id: int,
        message_id: int,
        status: str,
        external_error: str | None = None,
    ) -> None: ...

    async def download_reply_attachments(
        self, *, attachments: Sequence[dict[str, object]], maximum_total_bytes: int
    ) -> tuple[OutgoingAttachment, ...]: ...
