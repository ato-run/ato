from __future__ import annotations

import hashlib
from collections.abc import Callable

from .domain import (
    AuthorizationExpired,
    ChatwootTarget,
    Direction,
    HistoryExpired,
    ImportLimits,
    MailMessage,
    OutcomeUnknown,
    OutboxState,
    PermanentSendFailure,
    Preview,
    PublicReply,
)
from .mime import build_reply
from .ports import ChatwootPort, GmailPort
from .security import sanitize_email_html
from .store import Store


class ConnectionRejected(ValueError):
    pass


class MessageRejected(ValueError):
    pass


class SupportDesk:
    def __init__(
        self,
        *,
        store: Store,
        gmail: GmailPort,
        chatwoot: ChatwootPort,
        target: ChatwootTarget,
        mailbox_id: str,
        expected_send_as: str,
        limits: ImportLimits,
        support_predicate: Callable[[MailMessage], bool],
        allowed_agent_ids: frozenset[int],
    ) -> None:
        self.store = store
        self.gmail = gmail
        self.chatwoot = chatwoot
        self.target = target
        self.mailbox_id = mailbox_id
        self.expected_send_as = expected_send_as.lower()
        self.limits = limits
        self.support_predicate = support_predicate
        self.allowed_agent_ids = allowed_agent_ids

    async def verify_connection(self, granted_scopes: frozenset[str]) -> dict[str, object]:
        required = {
            "https://www.googleapis.com/auth/gmail.readonly",
            "https://www.googleapis.com/auth/gmail.send",
        }
        if not required.issubset(granted_scopes):
            raise ConnectionRejected("Google did not grant both required Gmail scopes")
        profile = await self.gmail.profile()
        aliases = await self.gmail.send_as()
        accepted = False
        for alias in aliases:
            address = str(alias.get("sendAsEmail", "")).lower()
            if address != self.expected_send_as:
                continue
            accepted = bool(alias.get("isPrimary")) or alias.get("verificationStatus") == "accepted"
            break
        if not accepted:
            raise ConnectionRejected(
                f"{self.expected_send_as} is not a primary or accepted send-as address"
            )
        profile_email = str(profile.get("emailAddress", "")).lower()
        if not profile_email:
            raise ConnectionRejected("Gmail profile did not identify the mailbox")
        self.store.upsert_mailbox(
            mailbox_id=self.mailbox_id,
            profile_email=profile_email,
            send_as_email=self.expected_send_as,
            status="connected",
            # The profile cursor is deliberately not activated here. A bounded
            # initial preview must be reviewed and committed first.
            history_id=None,
        )
        return {"profile_email": profile_email, "send_as_email": self.expected_send_as}

    async def preview_initial_sync(self) -> Preview:
        ids, history_id = await self.gmail.list_message_ids(
            query=self.limits.query,
            label_id=self.limits.label_id,
            limit=self.limits.max_messages,
        )
        accepted: list[MailMessage] = []
        rejected: list[str] = []
        for message_id in ids[: self.limits.max_messages]:
            message = await self.gmail.get_message(message_id)
            if self._is_allowed(message):
                accepted.append(self._safe_message(message))
            else:
                rejected.append(message_id)
        return Preview(tuple(accepted), tuple(rejected), history_id)

    async def commit_preview(self, preview: Preview) -> int:
        imported = 0
        for message in preview.messages:
            if await self._import_one(message):
                imported += 1
        self.store.set_history_id(self.mailbox_id, preview.next_history_id)
        return imported

    async def incremental_sync(self) -> tuple[int, bool]:
        cursor = self.store.history_id(self.mailbox_id)
        if not cursor:
            raise RuntimeError("initial sync preview has not been committed")
        try:
            ids, next_history_id = await self.gmail.history_message_ids(
                start_history_id=cursor, label_id=self.limits.label_id
            )
        except HistoryExpired:
            preview = await self.preview_initial_sync()
            return await self.commit_preview(preview), True
        imported = 0
        for message_id in ids:
            if self.store.has_message(self.mailbox_id, message_id):
                continue
            message = await self.gmail.get_message(message_id)
            if self._is_allowed(message) and await self._import_one(self._safe_message(message)):
                imported += 1
        self.store.set_history_id(self.mailbox_id, next_history_id)
        return imported, False

    async def handle_public_reply(self, reply: PublicReply) -> OutboxState | None:
        if reply.event != "message_created":
            return None
        if reply.message_type != "outgoing" or reply.private:
            return None
        if reply.account_id != self.target.account_id or reply.inbox_id != self.target.inbox_id:
            raise MessageRejected("Chatwoot webhook does not belong to the bound inbox")
        if reply.sender_id not in self.allowed_agent_ids:
            raise MessageRejected("Chatwoot sender is not an approved human agent")
        remote = await self.chatwoot.get_message(
            account_id=reply.account_id,
            conversation_id=reply.conversation_id,
            message_id=reply.chatwoot_message_id,
        )
        if int(remote.get("id", -1)) != reply.chatwoot_message_id:
            raise MessageRejected("Chatwoot message lookup did not match the webhook")
        if str(remote.get("message_type")) != "outgoing" or bool(remote.get("private")):
            raise MessageRejected("Chatwoot message is not a public outgoing reply")
        content_attributes = remote.get("content_attributes") or {}
        if isinstance(content_attributes, dict) and content_attributes.get(
            "ato_imported_from_gmail"
        ):
            return None
        remote_sender = remote.get("sender") or {}
        if not isinstance(remote_sender, dict) or int(remote_sender.get("id", -1)) != reply.sender_id:
            raise MessageRejected("Chatwoot message sender did not match the webhook")
        conversation = await self.chatwoot.get_conversation(
            account_id=reply.account_id, conversation_id=reply.conversation_id
        )
        inbox_id = int((conversation.get("inbox_id") or -1))
        if inbox_id != self.target.inbox_id:
            raise MessageRejected("Chatwoot conversation belongs to a different inbox")

        thread = self.store.thread_context(reply.conversation_id)
        downloaded_attachments = await self.chatwoot.download_reply_attachments(
            attachments=[dict(value) for value in reply.attachments],
            maximum_total_bytes=self.limits.attachment_max_bytes,
        )
        attachment_fingerprint = hashlib.sha256()
        for attachment in downloaded_attachments:
            attachment_fingerprint.update(attachment.filename.encode())
            attachment_fingerprint.update(b"\0")
            attachment_fingerprint.update(attachment.content_type.encode())
            attachment_fingerprint.update(b"\0")
            attachment_fingerprint.update(attachment.content)
        outbox_id, created = self.store.reserve_outbox(
            mailbox_id=thread.mailbox_id,
            chatwoot_message_id=reply.chatwoot_message_id,
            conversation_id=reply.conversation_id,
            content=reply.content,
            content_type=reply.content_type,
            attachment_fingerprint=attachment_fingerprint.hexdigest(),
        )
        if not created:
            return self.store.outbox_state(reply.chatwoot_message_id)

        text = reply.content
        html = sanitize_email_html(reply.content) if reply.content_type == "text/html" else None
        raw, rfc_message_id = build_reply(
            thread=thread,
            from_address=self.expected_send_as,
            text=text,
            html=html,
            outbox_key=outbox_id,
            attachments=downloaded_attachments,
        )
        self.store.transition_outbox(
            outbox_id,
            expected=OutboxState.PREPARED,
            state=OutboxState.SENDING,
            gmail_thread_id=thread.gmail_thread_id,
            rfc_message_id=rfc_message_id,
        )
        try:
            receipt = await self.gmail.send_raw(
                raw=raw, thread_id=thread.gmail_thread_id, request_id=outbox_id
            )
        except OutcomeUnknown as exc:
            self.store.transition_outbox(
                outbox_id,
                expected=OutboxState.SENDING,
                state=OutboxState.OUTCOME_UNKNOWN,
                error=str(exc),
            )
            await self.chatwoot.update_message_status(
                account_id=reply.account_id,
                conversation_id=reply.conversation_id,
                message_id=reply.chatwoot_message_id,
                status="failed",
                external_error=(
                    "Gmail outcome is unknown. Do not retry until Sent mail has "
                    "been reconciled or an operator has reviewed it."
                ),
            )
            return OutboxState.OUTCOME_UNKNOWN
        except AuthorizationExpired as exc:
            self.store.transition_outbox(
                outbox_id,
                expected=OutboxState.SENDING,
                state=OutboxState.REAUTHORIZATION_REQUIRED,
                error=str(exc),
            )
            raise
        except PermanentSendFailure as exc:
            self.store.transition_outbox(
                outbox_id,
                expected=OutboxState.SENDING,
                state=OutboxState.FAILED,
                error=str(exc),
            )
            await self.chatwoot.update_message_status(
                account_id=reply.account_id,
                conversation_id=reply.conversation_id,
                message_id=reply.chatwoot_message_id,
                status="failed",
                external_error="Gmail rejected the send request before accepting it.",
            )
            return OutboxState.FAILED
        self.store.transition_outbox(
            outbox_id,
            expected=OutboxState.SENDING,
            state=OutboxState.ACCEPTED_BY_GMAIL_API,
            gmail_message_id=receipt.gmail_message_id,
            gmail_thread_id=receipt.gmail_thread_id,
        )
        return OutboxState.ACCEPTED_BY_GMAIL_API

    async def _import_one(self, message: MailMessage) -> bool:
        if self.store.has_message(message.mailbox_id, message.gmail_message_id):
            return False
        if message.direction == Direction.OUTGOING_IMPORTED:
            self.store.reconcile_sent_message(message)
        conversation_id = self.store.conversation_id(
            message.mailbox_id, message.gmail_thread_id
        )
        resolved_conversation_id = await self.chatwoot.import_message(
            message=message, existing_conversation_id=conversation_id
        )
        self.store.record_import(
            message=message, chatwoot_conversation_id=resolved_conversation_id
        )
        return True

    def _is_allowed(self, message: MailMessage) -> bool:
        return (
            message.mailbox_id == self.mailbox_id
            and len(message.attachments) <= 32
            and all(
                attachment.size <= self.limits.attachment_max_bytes
                for attachment in message.attachments
            )
            and self.support_predicate(message)
        )

    @staticmethod
    def _safe_message(message: MailMessage) -> MailMessage:
        if message.html is None:
            return message
        return MailMessage(**{**message.__dict__, "html": sanitize_email_html(message.html)})
