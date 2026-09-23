from __future__ import annotations

from dataclasses import dataclass, field
from enum import StrEnum
from typing import Mapping, Sequence


class Direction(StrEnum):
    INCOMING = "incoming"
    OUTGOING_IMPORTED = "outgoing_imported"


class OutboxState(StrEnum):
    PREPARED = "prepared"
    SENDING = "sending"
    ACCEPTED_BY_GMAIL_API = "accepted_by_gmail_api"
    OUTCOME_UNKNOWN = "outcome_unknown"
    REAUTHORIZATION_REQUIRED = "reauthorization_required"
    FAILED = "failed"


@dataclass(frozen=True)
class Attachment:
    attachment_id: str
    filename: str
    content_type: str
    size: int
    content: bytes = field(default=b"", repr=False, compare=False)


@dataclass(frozen=True)
class OutgoingAttachment:
    filename: str
    content_type: str
    content: bytes = field(repr=False)


@dataclass(frozen=True)
class MailMessage:
    mailbox_id: str
    gmail_message_id: str
    gmail_thread_id: str
    rfc_message_id: str
    subject: str
    sender: str
    recipients: tuple[str, ...]
    reply_to: str | None
    references: tuple[str, ...]
    sent_at_ms: int
    text: str
    html: str | None = None
    label_ids: frozenset[str] = frozenset()
    headers: Mapping[str, str] = field(default_factory=dict)
    attachments: tuple[Attachment, ...] = ()
    direction: Direction = Direction.INCOMING


@dataclass(frozen=True)
class ImportLimits:
    query: str
    label_id: str | None
    max_messages: int
    attachment_max_bytes: int

    def __post_init__(self) -> None:
        if not self.query.strip():
            raise ValueError("a bounded Gmail query is required")
        if not 1 <= self.max_messages <= 500:
            raise ValueError("max_messages must be between 1 and 500")
        if not 1 <= self.attachment_max_bytes <= 25 * 1024 * 1024:
            raise ValueError("attachment_max_bytes is outside the supported range")


@dataclass(frozen=True)
class Preview:
    messages: tuple[MailMessage, ...]
    rejected_message_ids: tuple[str, ...]
    next_history_id: str


@dataclass(frozen=True)
class ChatwootTarget:
    account_id: int
    inbox_id: int
    inbox_identifier: str


@dataclass(frozen=True)
class PublicReply:
    delivery_id: str
    chatwoot_message_id: int
    conversation_id: int
    account_id: int
    inbox_id: int
    sender_id: int
    content: str
    content_type: str
    private: bool
    message_type: str
    event: str
    attachments: Sequence[Mapping[str, object]] = ()


@dataclass(frozen=True)
class ThreadContext:
    mailbox_id: str
    gmail_thread_id: str
    subject: str
    reply_to: str
    last_rfc_message_id: str
    references: tuple[str, ...]


@dataclass(frozen=True)
class SendReceipt:
    gmail_message_id: str
    gmail_thread_id: str
    history_id: str | None


class HistoryExpired(Exception):
    """The Gmail history cursor is no longer valid and needs bounded resync."""


class OutcomeUnknown(Exception):
    """The send request may have reached Gmail, so automatic retry is unsafe."""


class PermanentSendFailure(Exception):
    """Gmail rejected the request before accepting the message."""


class AuthorizationExpired(Exception):
    """Google no longer accepts the grant; all sync and send work must stop."""
