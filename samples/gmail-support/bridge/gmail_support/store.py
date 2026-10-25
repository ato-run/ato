from __future__ import annotations

import hashlib
import json
import sqlite3
import time
from contextlib import contextmanager
from dataclasses import asdict
from pathlib import Path
from typing import Iterator

from .domain import Direction, MailMessage, OutboxState, ThreadContext


SCHEMA = """
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS mailbox (
    mailbox_id TEXT PRIMARY KEY,
    profile_email TEXT NOT NULL,
    send_as_email TEXT NOT NULL,
    history_id TEXT,
    status TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS gmail_thread_map (
    mailbox_id TEXT NOT NULL,
    gmail_thread_id TEXT NOT NULL,
    chatwoot_conversation_id INTEGER NOT NULL UNIQUE,
    subject TEXT NOT NULL,
    reply_to TEXT NOT NULL,
    last_rfc_message_id TEXT NOT NULL,
    references_json TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (mailbox_id, gmail_thread_id)
);

CREATE TABLE IF NOT EXISTS gmail_message (
    mailbox_id TEXT NOT NULL,
    gmail_message_id TEXT NOT NULL,
    gmail_thread_id TEXT NOT NULL,
    rfc_message_id TEXT NOT NULL,
    direction TEXT NOT NULL,
    chatwoot_message_id INTEGER,
    metadata_json TEXT NOT NULL,
    imported_at INTEGER NOT NULL,
    PRIMARY KEY (mailbox_id, gmail_message_id)
);

CREATE TABLE IF NOT EXISTS webhook_delivery (
    delivery_id TEXT PRIMARY KEY,
    chatwoot_message_id INTEGER NOT NULL,
    received_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS outbox (
    outbox_id TEXT PRIMARY KEY,
    mailbox_id TEXT NOT NULL,
    chatwoot_message_id INTEGER NOT NULL UNIQUE,
    chatwoot_conversation_id INTEGER NOT NULL,
    revision_sha256 TEXT NOT NULL,
    state TEXT NOT NULL,
    gmail_message_id TEXT,
    gmail_thread_id TEXT,
    rfc_message_id TEXT,
    last_error TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS oauth_state (
    state_sha256 TEXT PRIMARY KEY,
    operation_user_id TEXT NOT NULL,
    ato_account_id TEXT NOT NULL,
    instance_id TEXT NOT NULL,
    binding_id TEXT NOT NULL,
    expected_mailbox TEXT NOT NULL,
    pkce_verifier_encrypted TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    consumed_at INTEGER
);
"""


class ConflictError(RuntimeError):
    pass


class Store:
    def __init__(self, path: str | Path) -> None:
        self.path = str(path)
        self._connection = sqlite3.connect(self.path, isolation_level=None)
        self._connection.row_factory = sqlite3.Row
        self._connection.executescript(SCHEMA)

    def close(self) -> None:
        self._connection.close()

    @contextmanager
    def transaction(self) -> Iterator[sqlite3.Connection]:
        self._connection.execute("BEGIN IMMEDIATE")
        try:
            yield self._connection
        except BaseException:
            self._connection.rollback()
            raise
        else:
            self._connection.commit()

    def recover_interrupted_sends(self) -> int:
        now = int(time.time())
        result = self._connection.execute(
            """
            UPDATE outbox
               SET state = ?, last_error = ?, updated_at = ?
             WHERE state = ?
            """,
            (
                OutboxState.OUTCOME_UNKNOWN,
                "bridge restarted while the Gmail request was in flight",
                now,
                OutboxState.SENDING,
            ),
        )
        return result.rowcount

    def upsert_mailbox(
        self,
        *,
        mailbox_id: str,
        profile_email: str,
        send_as_email: str,
        status: str,
        history_id: str | None = None,
    ) -> None:
        self._connection.execute(
            """
            INSERT INTO mailbox (
              mailbox_id, profile_email, send_as_email, history_id, status, updated_at
            ) VALUES (?, ?, ?, ?, ?, ?)
            ON CONFLICT(mailbox_id) DO UPDATE SET
              profile_email = excluded.profile_email,
              send_as_email = excluded.send_as_email,
              history_id = COALESCE(excluded.history_id, mailbox.history_id),
              status = excluded.status,
              updated_at = excluded.updated_at
            """,
            (mailbox_id, profile_email, send_as_email, history_id, status, int(time.time())),
        )

    def history_id(self, mailbox_id: str) -> str | None:
        row = self._connection.execute(
            "SELECT history_id FROM mailbox WHERE mailbox_id = ?", (mailbox_id,)
        ).fetchone()
        return None if row is None else row["history_id"]

    def set_history_id(self, mailbox_id: str, history_id: str) -> None:
        result = self._connection.execute(
            "UPDATE mailbox SET history_id = ?, updated_at = ? WHERE mailbox_id = ?",
            (history_id, int(time.time()), mailbox_id),
        )
        if result.rowcount != 1:
            raise KeyError(f"unknown mailbox: {mailbox_id}")

    def has_message(self, mailbox_id: str, gmail_message_id: str) -> bool:
        return (
            self._connection.execute(
                "SELECT 1 FROM gmail_message WHERE mailbox_id = ? AND gmail_message_id = ?",
                (mailbox_id, gmail_message_id),
            ).fetchone()
            is not None
        )

    def conversation_id(self, mailbox_id: str, gmail_thread_id: str) -> int | None:
        row = self._connection.execute(
            """
            SELECT chatwoot_conversation_id
              FROM gmail_thread_map
             WHERE mailbox_id = ? AND gmail_thread_id = ?
            """,
            (mailbox_id, gmail_thread_id),
        ).fetchone()
        return None if row is None else int(row["chatwoot_conversation_id"])

    def record_import(
        self,
        *,
        message: MailMessage,
        chatwoot_conversation_id: int,
        chatwoot_message_id: int | None = None,
    ) -> None:
        reply_to = message.reply_to or message.sender
        preserve_reply_target = message.direction == Direction.OUTGOING_IMPORTED
        with self.transaction() as connection:
            metadata = asdict(message)
            metadata["attachments"] = [
                {
                    "attachment_id": item.attachment_id,
                    "filename": item.filename,
                    "content_type": item.content_type,
                    "size": item.size,
                }
                for item in message.attachments
            ]
            connection.execute(
                """
                INSERT INTO gmail_thread_map (
                  mailbox_id, gmail_thread_id, chatwoot_conversation_id, subject,
                  reply_to, last_rfc_message_id, references_json, updated_at
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(mailbox_id, gmail_thread_id) DO UPDATE SET
                  subject = excluded.subject,
                  reply_to = CASE WHEN ? THEN gmail_thread_map.reply_to ELSE excluded.reply_to END,
                  last_rfc_message_id = excluded.last_rfc_message_id,
                  references_json = excluded.references_json,
                  updated_at = excluded.updated_at
                """,
                (
                    message.mailbox_id,
                    message.gmail_thread_id,
                    chatwoot_conversation_id,
                    message.subject,
                    reply_to,
                    message.rfc_message_id,
                    json.dumps(message.references),
                    int(time.time()),
                    preserve_reply_target,
                ),
            )
            connection.execute(
                """
                INSERT INTO gmail_message (
                  mailbox_id, gmail_message_id, gmail_thread_id, rfc_message_id,
                  direction, chatwoot_message_id, metadata_json, imported_at
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    message.mailbox_id,
                    message.gmail_message_id,
                    message.gmail_thread_id,
                    message.rfc_message_id,
                    message.direction,
                    chatwoot_message_id,
                    json.dumps(metadata, default=list, sort_keys=True),
                    int(time.time()),
                ),
            )

    def thread_context(self, conversation_id: int) -> ThreadContext:
        row = self._connection.execute(
            """
            SELECT mailbox_id, gmail_thread_id, subject, reply_to,
                   last_rfc_message_id, references_json
              FROM gmail_thread_map
             WHERE chatwoot_conversation_id = ?
            """,
            (conversation_id,),
        ).fetchone()
        if row is None:
            raise KeyError(f"conversation {conversation_id} is not mapped to Gmail")
        return ThreadContext(
            mailbox_id=row["mailbox_id"],
            gmail_thread_id=row["gmail_thread_id"],
            subject=row["subject"],
            reply_to=row["reply_to"],
            last_rfc_message_id=row["last_rfc_message_id"],
            references=tuple(json.loads(row["references_json"])),
        )

    def reserve_delivery(self, delivery_id: str, chatwoot_message_id: int) -> bool:
        try:
            self._connection.execute(
                "INSERT INTO webhook_delivery VALUES (?, ?, ?)",
                (delivery_id, chatwoot_message_id, int(time.time())),
            )
        except sqlite3.IntegrityError:
            return False
        return True

    def delivery_seen(self, delivery_id: str) -> bool:
        return (
            self._connection.execute(
                "SELECT 1 FROM webhook_delivery WHERE delivery_id = ?", (delivery_id,)
            ).fetchone()
            is not None
        )

    def reserve_outbox(
        self,
        *,
        mailbox_id: str,
        chatwoot_message_id: int,
        conversation_id: int,
        content: str,
        content_type: str,
        attachment_fingerprint: str,
    ) -> tuple[str, bool]:
        revision = hashlib.sha256(
            (content_type + "\0" + content + "\0" + attachment_fingerprint).encode()
        ).hexdigest()
        outbox_id = f"cw-{chatwoot_message_id}-{revision[:16]}"
        now = int(time.time())
        try:
            self._connection.execute(
                """
                INSERT INTO outbox (
                  outbox_id, mailbox_id, chatwoot_message_id, chatwoot_conversation_id,
                  revision_sha256, state, created_at, updated_at
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    outbox_id,
                    mailbox_id,
                    chatwoot_message_id,
                    conversation_id,
                    revision,
                    OutboxState.PREPARED,
                    now,
                    now,
                ),
            )
            return outbox_id, True
        except sqlite3.IntegrityError:
            row = self._connection.execute(
                "SELECT outbox_id, revision_sha256 FROM outbox WHERE chatwoot_message_id = ?",
                (chatwoot_message_id,),
            ).fetchone()
            if row is None or row["revision_sha256"] != revision:
                raise ConflictError("Chatwoot message revision does not match the outbox")
            return str(row["outbox_id"]), False

    def transition_outbox(
        self,
        outbox_id: str,
        *,
        expected: OutboxState,
        state: OutboxState,
        gmail_message_id: str | None = None,
        gmail_thread_id: str | None = None,
        rfc_message_id: str | None = None,
        error: str | None = None,
    ) -> None:
        result = self._connection.execute(
            """
            UPDATE outbox
               SET state = ?, gmail_message_id = COALESCE(?, gmail_message_id),
                   gmail_thread_id = COALESCE(?, gmail_thread_id),
                   rfc_message_id = COALESCE(?, rfc_message_id),
                   last_error = ?, updated_at = ?
             WHERE outbox_id = ? AND state = ?
            """,
            (
                state,
                gmail_message_id,
                gmail_thread_id,
                rfc_message_id,
                error,
                int(time.time()),
                outbox_id,
                expected,
            ),
        )
        if result.rowcount != 1:
            raise ConflictError(f"outbox {outbox_id} is no longer {expected}")

    def outbox_state(self, chatwoot_message_id: int) -> OutboxState | None:
        row = self._connection.execute(
            "SELECT state FROM outbox WHERE chatwoot_message_id = ?",
            (chatwoot_message_id,),
        ).fetchone()
        return None if row is None else OutboxState(row["state"])

    def reconcile_sent_message(self, message: MailMessage) -> bool:
        outbox_id = message.headers.get("X-Ato-Outbox-ID")
        if not outbox_id or not message.rfc_message_id:
            return False
        result = self._connection.execute(
            """
            UPDATE outbox
               SET state = ?, gmail_message_id = ?, gmail_thread_id = ?,
                   last_error = NULL, updated_at = ?
             WHERE outbox_id = ? AND mailbox_id = ?
               AND gmail_thread_id = ? AND rfc_message_id = ?
               AND state IN (?, ?, ?)
            """,
            (
                OutboxState.ACCEPTED_BY_GMAIL_API,
                message.gmail_message_id,
                message.gmail_thread_id,
                int(time.time()),
                outbox_id,
                message.mailbox_id,
                message.gmail_thread_id,
                message.rfc_message_id,
                OutboxState.OUTCOME_UNKNOWN,
                OutboxState.REAUTHORIZATION_REQUIRED,
                OutboxState.SENDING,
            ),
        )
        return result.rowcount == 1

    def create_oauth_state(
        self,
        *,
        state: str,
        operation_user_id: str,
        ato_account_id: str,
        instance_id: str,
        binding_id: str,
        expected_mailbox: str,
        encrypted_verifier: str,
        expires_at: int,
    ) -> None:
        self._connection.execute(
            """
            INSERT INTO oauth_state (
              state_sha256, operation_user_id, ato_account_id, instance_id,
              binding_id, expected_mailbox, pkce_verifier_encrypted, expires_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            """,
            (
                hashlib.sha256(state.encode()).hexdigest(),
                operation_user_id,
                ato_account_id,
                instance_id,
                binding_id,
                expected_mailbox,
                encrypted_verifier,
                expires_at,
            ),
        )

    def consume_oauth_state(self, state: str, now: int) -> dict[str, object]:
        digest = hashlib.sha256(state.encode()).hexdigest()
        with self.transaction() as connection:
            row = connection.execute(
                """
                SELECT * FROM oauth_state
                 WHERE state_sha256 = ? AND consumed_at IS NULL AND expires_at > ?
                """,
                (digest, now),
            ).fetchone()
            if row is None:
                raise ConflictError("OAuth state is invalid, expired, or already used")
            connection.execute(
                "UPDATE oauth_state SET consumed_at = ? WHERE state_sha256 = ?",
                (now, digest),
            )
            return dict(row)
