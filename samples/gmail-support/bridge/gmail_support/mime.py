from __future__ import annotations

import base64
import re
from email.message import EmailMessage
from email.policy import SMTP
from email.utils import formataddr, make_msgid, parseaddr

from .domain import OutgoingAttachment, ThreadContext


_HEADER_BREAK = re.compile(r"[\r\n]")


def _address(value: str) -> str:
    if _HEADER_BREAK.search(value):
        raise ValueError("address contains a header break")
    name, address = parseaddr(value)
    if not address or "@" not in address:
        raise ValueError("invalid email address")
    return formataddr((name, address))


def build_reply(
    *,
    thread: ThreadContext,
    from_address: str,
    text: str,
    html: str | None,
    outbox_key: str,
    attachments: tuple[OutgoingAttachment, ...] = (),
) -> tuple[str, str]:
    if not thread.subject or not thread.last_rfc_message_id:
        raise ValueError("threading headers are incomplete")
    message = EmailMessage(policy=SMTP)
    message["From"] = _address(from_address)
    message["To"] = _address(thread.reply_to)
    message["Subject"] = thread.subject
    message["In-Reply-To"] = thread.last_rfc_message_id
    references = (*thread.references, thread.last_rfc_message_id)
    message["References"] = " ".join(dict.fromkeys(references))
    message["Message-ID"] = make_msgid(idstring=outbox_key, domain="support.ato.run")
    message["X-Ato-Outbox-ID"] = outbox_key
    message.set_content(text)
    if html:
        message.add_alternative(html, subtype="html")
    for attachment in attachments:
        maintype, _, subtype = attachment.content_type.partition("/")
        if not maintype or not subtype:
            maintype, subtype = "application", "octet-stream"
        message.add_attachment(
            attachment.content,
            maintype=maintype,
            subtype=subtype,
            filename=attachment.filename,
        )
    raw = base64.urlsafe_b64encode(message.as_bytes()).decode("ascii").rstrip("=")
    return raw, message["Message-ID"]
