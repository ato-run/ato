from __future__ import annotations

import base64
import email
import tempfile
import unittest
from pathlib import Path

from gmail_support.domain import (
    ChatwootTarget,
    Direction,
    HistoryExpired,
    ImportLimits,
    MailMessage,
    OutcomeUnknown,
    OutboxState,
    PermanentSendFailure,
    PublicReply,
    SendReceipt,
)
from gmail_support.security import (
    internal_chatwoot_attachment_url,
    sanitize_email_html,
    verify_chatwoot_signature,
)
from gmail_support.service import SupportDesk
from gmail_support.store import Store


class FakeGmail:
    def __init__(self, messages: list[MailMessage]) -> None:
        self.messages = {message.gmail_message_id: message for message in messages}
        self.sent: list[tuple[str, str, str]] = []
        self.unknown = False
        self.permanent_failure = False
        self.expire_history = False

    async def profile(self):
        return {"emailAddress": "operator@ato.run", "historyId": "100"}

    async def send_as(self):
        return [{"sendAsEmail": "support@ato.run", "verificationStatus": "accepted"}]

    async def list_message_ids(self, *, query, label_id, limit):
        return list(self.messages)[:limit], "101"

    async def history_message_ids(self, *, start_history_id, label_id):
        if self.expire_history:
            raise HistoryExpired
        return list(self.messages), "102"

    async def get_message(self, message_id):
        return self.messages[message_id]

    async def send_raw(self, *, raw, thread_id, request_id):
        self.sent.append((raw, thread_id, request_id))
        if self.unknown:
            raise OutcomeUnknown("timeout after request upload")
        if self.permanent_failure:
            raise PermanentSendFailure("rejected before acceptance")
        return SendReceipt("gmail-sent-1", thread_id, "103")


class FakeChatwoot:
    def __init__(self) -> None:
        self.imports: list[MailMessage] = []
        self.status_updates: list[dict[str, object]] = []
        self.next_conversation = 900

    async def import_message(self, *, message, existing_conversation_id):
        self.imports.append(message)
        if existing_conversation_id is not None:
            return existing_conversation_id
        self.next_conversation += 1
        return self.next_conversation

    async def get_message(self, *, account_id, conversation_id, message_id):
        return {
            "id": message_id,
            "message_type": "outgoing",
            "private": False,
            "sender": {"id": 33, "type": "user"},
        }

    async def get_conversation(self, *, account_id, conversation_id):
        return {"id": conversation_id, "inbox_id": 22}

    async def update_message_status(
        self,
        *,
        account_id,
        conversation_id,
        message_id,
        status,
        external_error=None,
    ):
        self.status_updates.append(
            {
                "account_id": account_id,
                "conversation_id": conversation_id,
                "message_id": message_id,
                "status": status,
                "external_error": external_error,
            }
        )

    async def download_reply_attachments(self, *, attachments, maximum_total_bytes):
        return ()


def message(identifier: str = "m1", thread: str = "t1", *, allowed: bool = True) -> MailMessage:
    headers = {"Delivered-To": "support@ato.run" if allowed else "personal@ato.run"}
    return MailMessage(
        mailbox_id="support-primary",
        gmail_message_id=identifier,
        gmail_thread_id=thread,
        rfc_message_id=f"<{identifier}@sender.test>",
        subject="お問い合わせ",
        sender="利用者 <user@example.test>",
        recipients=("support@ato.run",),
        reply_to="user@example.test",
        references=(),
        sent_at_ms=1000,
        text="本文",
        html='<p>本文</p><img src="https://tracker.test/pixel">',
        headers=headers,
        direction=Direction.INCOMING,
    )


class SupportDeskTest(unittest.IsolatedAsyncioTestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.store = Store(Path(self.directory.name) / "bridge.sqlite3")
        self.gmail = FakeGmail([message(), message("m2", allowed=False)])
        self.chatwoot = FakeChatwoot()
        self.service = SupportDesk(
            store=self.store,
            gmail=self.gmail,
            chatwoot=self.chatwoot,
            target=ChatwootTarget(11, 22, "inbox-token"),
            mailbox_id="support-primary",
            expected_send_as="support@ato.run",
            limits=ImportLimits("newer_than:30d", None, 100, 10 * 1024 * 1024),
            support_predicate=lambda item: item.headers.get("Delivered-To") == "support@ato.run",
            allowed_agent_ids=frozenset({33}),
        )

    def tearDown(self) -> None:
        self.store.close()
        self.directory.cleanup()

    async def connect_and_import(self) -> None:
        await self.service.verify_connection(
            frozenset(
                {
                    "https://www.googleapis.com/auth/gmail.readonly",
                    "https://www.googleapis.com/auth/gmail.send",
                }
            )
        )
        preview = await self.service.preview_initial_sync()
        self.assertEqual([item.gmail_message_id for item in preview.messages], ["m1"])
        self.assertEqual(preview.rejected_message_ids, ("m2",))
        self.assertNotIn("img", preview.messages[0].html or "")
        self.assertEqual(await self.service.commit_preview(preview), 1)

    async def test_preview_filters_before_storage_and_deduplicates(self) -> None:
        await self.connect_and_import()
        self.assertEqual(len(self.chatwoot.imports), 1)
        imported, resynced = await self.service.incremental_sync()
        self.assertEqual((imported, resynced), (0, False))

    async def test_expired_history_uses_same_bounded_resync(self) -> None:
        await self.connect_and_import()
        self.gmail.expire_history = True
        imported, resynced = await self.service.incremental_sync()
        self.assertEqual((imported, resynced), (0, True))

    async def test_private_note_is_not_sent(self) -> None:
        await self.connect_and_import()
        state = await self.service.handle_public_reply(self.reply(private=True))
        self.assertIsNone(state)
        self.assertEqual(self.gmail.sent, [])

    async def test_public_reply_is_sent_once_and_keeps_thread_headers(self) -> None:
        await self.connect_and_import()
        reply = self.reply()
        self.assertEqual(
            await self.service.handle_public_reply(reply), OutboxState.ACCEPTED_BY_GMAIL_API
        )
        self.assertEqual(
            await self.service.handle_public_reply(reply), OutboxState.ACCEPTED_BY_GMAIL_API
        )
        self.assertEqual(len(self.gmail.sent), 1)
        raw = self.gmail.sent[0][0]
        decoded = base64.urlsafe_b64decode(raw + "=" * (-len(raw) % 4)).decode()
        self.assertIn("In-Reply-To: <m1@sender.test>", decoded)
        self.assertIn("Subject:", decoded)

    async def test_changed_reply_revision_cannot_reuse_a_chatwoot_message(self) -> None:
        await self.connect_and_import()
        reply = self.reply()
        await self.service.handle_public_reply(reply)
        changed = PublicReply(**{**reply.__dict__, "content": "changed after approval"})
        with self.assertRaises(RuntimeError):
            await self.service.handle_public_reply(changed)
        self.assertEqual(len(self.gmail.sent), 1)

    async def test_ambiguous_send_is_held_without_retry(self) -> None:
        await self.connect_and_import()
        self.gmail.unknown = True
        reply = self.reply()
        self.assertEqual(
            await self.service.handle_public_reply(reply), OutboxState.OUTCOME_UNKNOWN
        )
        self.gmail.unknown = False
        self.assertEqual(
            await self.service.handle_public_reply(reply), OutboxState.OUTCOME_UNKNOWN
        )
        self.assertEqual(len(self.gmail.sent), 1)
        self.assertEqual(self.chatwoot.status_updates[-1]["status"], "failed")
        self.assertIn(
            "Do not retry",
            str(self.chatwoot.status_updates[-1]["external_error"]),
        )

    async def test_permanent_send_failure_is_visible_in_chatwoot(self) -> None:
        await self.connect_and_import()
        self.gmail.permanent_failure = True
        reply = self.reply()
        self.assertEqual(
            await self.service.handle_public_reply(reply), OutboxState.FAILED
        )
        self.assertEqual(self.chatwoot.status_updates[-1]["status"], "failed")
        self.assertEqual(len(self.gmail.sent), 1)

    async def test_sent_mailbox_observation_reconciles_an_unknown_send(self) -> None:
        await self.connect_and_import()
        self.gmail.unknown = True
        reply = self.reply()
        self.assertEqual(
            await self.service.handle_public_reply(reply), OutboxState.OUTCOME_UNKNOWN
        )
        raw_value, thread_id, outbox_id = self.gmail.sent[0]
        raw = base64.urlsafe_b64decode(raw_value + "=" * (-len(raw_value) % 4))
        parsed = email.message_from_bytes(raw)
        observed = MailMessage(
            mailbox_id="support-primary",
            gmail_message_id="gmail-observed-sent",
            gmail_thread_id=thread_id,
            rfc_message_id=str(parsed["Message-ID"]),
            subject="お問い合わせ",
            sender="support@ato.run",
            recipients=("user@example.test",),
            reply_to=None,
            references=("<m1@sender.test>",),
            sent_at_ms=2000,
            text="ご連絡ありがとうございます。",
            label_ids=frozenset({"SENT"}),
            headers={
                "Delivered-To": "support@ato.run",
                "X-Ato-Outbox-ID": outbox_id,
            },
            direction=Direction.OUTGOING_IMPORTED,
        )
        self.gmail.messages[observed.gmail_message_id] = observed
        self.gmail.unknown = False
        imported, _ = await self.service.incremental_sync()
        self.assertEqual(imported, 1)
        self.assertEqual(
            self.store.outbox_state(reply.chatwoot_message_id),
            OutboxState.ACCEPTED_BY_GMAIL_API,
        )
        context = self.store.thread_context(reply.conversation_id)
        self.assertEqual(context.reply_to, "user@example.test")
        self.assertEqual(context.last_rfc_message_id, parsed["Message-ID"])

    @staticmethod
    def reply(*, private: bool = False) -> PublicReply:
        return PublicReply(
            delivery_id="delivery-1",
            chatwoot_message_id=44,
            conversation_id=901,
            account_id=11,
            inbox_id=22,
            sender_id=33,
            content="ご連絡ありがとうございます。",
            content_type="text",
            private=private,
            message_type="outgoing",
            event="message_created",
        )


class SecurityTest(unittest.TestCase):
    def test_chatwoot_signature_checks_raw_body_and_time(self) -> None:
        import hashlib
        import hmac

        body = b'{"event":"message_created"}'
        timestamp = "1000"
        signature = "sha256=" + hmac.new(
            b"secret", timestamp.encode() + b"." + body, hashlib.sha256
        ).hexdigest()
        verify_chatwoot_signature(
            raw_body=body,
            timestamp=timestamp,
            signature=signature,
            secret="secret",
            now=1000,
        )
        with self.assertRaises(ValueError):
            verify_chatwoot_signature(
                raw_body=body + b" ",
                timestamp=timestamp,
                signature=signature,
                secret="secret",
                now=1000,
            )

    def test_html_drops_remote_images_and_scripts(self) -> None:
        safe = sanitize_email_html(
            '<p>hello <strong>world</strong></p><img src="https://track.test/x">'
            '<script>alert(1)</script><style>body{display:none}</style>'
        )
        self.assertEqual(safe, "<p>hello <strong>world</strong></p>")

    def test_chatwoot_attachment_url_is_pinned_to_the_internal_origin(self) -> None:
        self.assertEqual(
            internal_chatwoot_attachment_url(
                "https://support.example.test/rails/active_storage/file?token=1",
                internal_base_url="http://127.0.0.1:3000",
                public_base_url="https://support.example.test",
            ),
            "http://127.0.0.1:3000/rails/active_storage/file?token=1",
        )
        with self.assertRaises(ValueError):
            internal_chatwoot_attachment_url(
                "https://attacker.example/file",
                internal_base_url="http://127.0.0.1:3000",
                public_base_url="https://support.example.test",
            )


if __name__ == "__main__":
    unittest.main()
