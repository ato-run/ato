from __future__ import annotations

import asyncio
import json
import logging
import os
from contextlib import suppress
from email.utils import getaddresses
from typing import Any
from urllib.parse import urlsplit

import aiohttp
from aiohttp import WSMsgType, web
from multidict import CIMultiDict

from .chatwoot import ChatwootClient
from .config import Config
from .domain import AuthorizationExpired, MailMessage, Preview, PublicReply
from .google_gmail import GoogleGmail
from .oauth import OAuthCoordinator, SCOPES
from .restricted_proxy import RestrictedProxyServer, start_restricted_proxy
from .security import WebhookRejected, verify_chatwoot_signature
from .service import MessageRejected, SupportDesk
from .store import Store


LOGGER = logging.getLogger("gmail_support")


class Runtime:
    def __init__(self, config: Config) -> None:
        self.config = config
        config.database_path.parent.mkdir(parents=True, exist_ok=True)
        self.store = Store(config.database_path)
        recovered = self.store.recover_interrupted_sends()
        if recovered:
            LOGGER.warning("marked %d interrupted outbox sends as outcome_unknown", recovered)
        self.chatwoot = ChatwootClient(
            base_url=config.chatwoot_url,
            api_token=config.chatwoot_api_token,
            target=config.target,
            public_hmac_token=config.chatwoot_public_hmac_token,
            public_base_url=config.app_origin,
        )
        self.service: SupportDesk | None = None
        self.sync_task: asyncio.Task[None] | None = None
        self.last_sync_error: str | None = None
        self.initial_preview_required = False
        self.authorization_expired = False
        self.pending_preview: Preview | None = None
        self.restricted_proxy: RestrictedProxyServer | None = None
        self.oauth = OAuthCoordinator(config, self.store)
        if config.gmail_oauth_json:
            self.ensure_proxy()
            self.service = self._new_service(config.gmail_oauth_json)

    def _new_service(self, credential_json: str) -> SupportDesk:
        gmail = GoogleGmail(
            mailbox_id=self.config.mailbox_id,
            credential_json=credential_json,
            maximum_message_bytes=self.config.maximum_message_bytes,
        )
        return SupportDesk(
            store=self.store,
            gmail=gmail,
            chatwoot=self.chatwoot,
            target=self.config.target,
            mailbox_id=self.config.mailbox_id,
            expected_send_as=self.config.support_address,
            limits=self.config.limits,
            support_predicate=self._is_support_message,
            allowed_agent_ids=self.config.allowed_agent_ids,
        )

    def ensure_proxy(self) -> None:
        if self.restricted_proxy:
            return
        if not self.config.egress_socks_url or not self.config.restricted_proxy_target:
            raise RuntimeError("restricted HTTPS egress is not configured")
        update_host = urlsplit(self.config.refreshable_gmail_binding.update_url).hostname
        if not update_host:
            raise RuntimeError("Binding update URL is invalid")
        self.restricted_proxy = start_restricted_proxy(
            socks_url=self.config.egress_socks_url,
            upstream_authority=self.config.restricted_proxy_target,
            upstream_authorization=self.config.restricted_proxy_authorization,
            additional_allowed_hosts=frozenset({update_host}),
        )
        os.environ["HTTPS_PROXY"] = "http://127.0.0.1:18080"
        os.environ["https_proxy"] = "http://127.0.0.1:18080"

    async def activate_credentials(self, credential_json: str) -> None:
        self.ensure_proxy()
        candidate = self._new_service(credential_json)
        await candidate.verify_connection(frozenset(SCOPES))
        await self.oauth.persist_binding(credential_json)
        self.service = candidate
        self.initial_preview_required = True
        self.authorization_expired = False

    def _is_support_message(self, message: MailMessage) -> bool:
        if self.store.conversation_id(message.mailbox_id, message.gmail_thread_id) is not None:
            return True
        if self.config.limits.label_id and self.config.limits.label_id not in message.label_ids:
            return False
        header_values = [
            message.headers.get("Delivered-To", ""),
            message.headers.get("X-Original-To", ""),
            message.headers.get("To", ""),
        ]
        delivered = {address.lower() for _, address in getaddresses(header_values)}
        return self.config.support_address in delivered

    async def start(self) -> None:
        if self.service is None:
            return
        scopes = frozenset(
            json.loads(self.config.gmail_oauth_json or "{}").get("scopes", [])
        )
        await self.service.verify_connection(scopes)
        if self.store.history_id(self.config.mailbox_id):
            self.sync_task = asyncio.create_task(self._sync_loop())
        else:
            self.initial_preview_required = True

    async def stop(self) -> None:
        if self.sync_task:
            self.sync_task.cancel()
            with suppress(asyncio.CancelledError):
                await self.sync_task
        self.store.close()
        if self.restricted_proxy:
            self.restricted_proxy.shutdown()
            self.restricted_proxy.server_close()

    async def _sync_loop(self) -> None:
        while True:
            try:
                assert self.service is not None
                await self.service.incremental_sync()
                self.last_sync_error = None
            except asyncio.CancelledError:
                raise
            except AuthorizationExpired:
                self.authorization_expired = True
                self.last_sync_error = "AuthorizationExpired"
                LOGGER.warning("Gmail authorization expired; synchronization is stopped")
                return
            except Exception as exc:  # keep polling stopped state visible without leaking secrets
                self.last_sync_error = type(exc).__name__
                LOGGER.exception("Gmail synchronization stopped for this interval")
            await asyncio.sleep(self.config.sync_interval_seconds)


def create_application(runtime: Runtime) -> web.Application:
    application = web.Application(client_max_size=runtime.config.maximum_message_bytes)
    application["runtime"] = runtime
    application.router.add_get("/health", health)
    application.router.add_get("/__ato/gmail/preview", initial_preview)
    application.router.add_post("/__ato/gmail/preview/commit", commit_initial_preview)
    application.router.add_post("/__ato/gmail/oauth/start", oauth_start)
    application.router.add_get("/__ato/gmail/oauth/callback", oauth_callback)
    application.router.add_post("/__ato/chatwoot/webhook", chatwoot_webhook)
    application.router.add_route("*", "/{path:.*}", chatwoot_proxy)

    async def startup(_: web.Application) -> None:
        await runtime.start()

    async def cleanup(_: web.Application) -> None:
        await runtime.stop()

    application.on_startup.append(startup)
    application.on_cleanup.append(cleanup)
    return application


async def health(request: web.Request) -> web.Response:
    runtime: Runtime = request.app["runtime"]
    try:
        async with aiohttp.ClientSession(timeout=aiohttp.ClientTimeout(total=2)) as session:
            async with session.get(runtime.config.chatwoot_url, allow_redirects=False) as upstream:
                if upstream.status >= 500:
                    raise RuntimeError("Chatwoot is not ready")
    except (aiohttp.ClientError, asyncio.TimeoutError, RuntimeError):
        return web.json_response({"status": "chatwoot_starting"}, status=503)
    if runtime.service is None:
        status = "connection_waiting"
    elif runtime.authorization_expired:
        status = "reauthorization_required"
    elif runtime.initial_preview_required:
        status = "preview_required"
    elif runtime.last_sync_error:
        status = "sync_degraded"
    else:
        status = "ready"
    return web.json_response(
        {
            "status": status,
            "mailbox_id": runtime.config.mailbox_id,
            "send_as": runtime.config.support_address,
            "last_sync_error": runtime.last_sync_error,
        }
    )


async def chatwoot_webhook(request: web.Request) -> web.Response:
    runtime: Runtime = request.app["runtime"]
    raw = await request.read()
    try:
        verify_chatwoot_signature(
            raw_body=raw,
            timestamp=request.headers.get("X-Chatwoot-Timestamp"),
            signature=request.headers.get("X-Chatwoot-Signature"),
            secret=runtime.config.chatwoot_webhook_secret,
        )
    except WebhookRejected as exc:
        raise web.HTTPUnauthorized(text=str(exc)) from exc
    delivery_id = request.headers.get("X-Chatwoot-Delivery")
    if not delivery_id:
        raise web.HTTPUnauthorized(text="missing Chatwoot delivery ID")
    if runtime.store.delivery_seen(delivery_id):
        return web.json_response({"status": "duplicate"})
    try:
        payload = json.loads(raw)
        message_id = int(payload["id"])
        if (
            payload.get("event") != "message_created"
            or payload.get("message_type") != "outgoing"
            or bool(payload.get("private", False))
        ):
            runtime.store.reserve_delivery(delivery_id, message_id)
            return web.json_response({"status": "ignored"})
        if runtime.service is None or runtime.authorization_expired:
            # A public reply created while sending is disabled must never be
            # delivered later merely because Chatwoot retries this webhook
            # after OAuth is restored. The failed UI action needs a new,
            # explicit human send after reconnection.
            runtime.store.reserve_delivery(delivery_id, message_id)
            raise web.HTTPServiceUnavailable(
                text="Gmail connection is waiting for authorization"
            )
        reply = _public_reply(delivery_id, payload)
        state = await runtime.service.handle_public_reply(reply)
    except AuthorizationExpired as exc:
        runtime.authorization_expired = True
        raise web.HTTPServiceUnavailable(text="Gmail reauthorization is required") from exc
    except (KeyError, TypeError, ValueError, MessageRejected) as exc:
        raise web.HTTPBadRequest(text=str(exc)) from exc
    runtime.store.reserve_delivery(delivery_id, reply.chatwoot_message_id)
    return web.json_response({"status": "ignored" if state is None else state})


def _require_admin(request: web.Request, runtime: Runtime) -> None:
    expected = f"Bearer {runtime.config.bridge_admin_token}"
    if request.headers.get("Authorization") != expected:
        raise web.HTTPUnauthorized(text="admin authorization is required")


async def initial_preview(request: web.Request) -> web.Response:
    runtime: Runtime = request.app["runtime"]
    _require_admin(request, runtime)
    if runtime.service is None:
        raise web.HTTPServiceUnavailable(text="Gmail connection is waiting for authorization")
    runtime.pending_preview = await runtime.service.preview_initial_sync()
    return web.json_response(
        {
            "next_history_id": runtime.pending_preview.next_history_id,
            "accepted": [
                {
                    "gmail_message_id": message.gmail_message_id,
                    "gmail_thread_id": message.gmail_thread_id,
                    "sender": message.sender,
                    "subject": message.subject,
                    "sent_at_ms": message.sent_at_ms,
                    "attachment_count": len(message.attachments),
                    "direction": message.direction,
                }
                for message in runtime.pending_preview.messages
            ],
            "rejected_message_ids": runtime.pending_preview.rejected_message_ids,
        }
    )


async def oauth_start(request: web.Request) -> web.Response:
    runtime: Runtime = request.app["runtime"]
    _require_admin(request, runtime)
    try:
        runtime.ensure_proxy()
        started = runtime.oauth.start()
    except (RuntimeError, ValueError) as exc:
        raise web.HTTPServiceUnavailable(text=str(exc)) from exc
    return web.json_response({"authorization_url": started.authorization_url})


async def oauth_callback(request: web.Request) -> web.Response:
    runtime: Runtime = request.app["runtime"]
    state = request.query.get("state")
    code = request.query.get("code")
    if not state or not code:
        raise web.HTTPBadRequest(text="OAuth callback is incomplete")
    try:
        credential_json = await runtime.oauth.exchange(state=state, code=code)
        await runtime.activate_credentials(credential_json)
    except (RuntimeError, ValueError) as exc:
        LOGGER.warning("Gmail OAuth callback was rejected: %s", type(exc).__name__)
        raise web.HTTPBadRequest(text="Gmail connection could not be completed") from exc
    return web.HTTPFound("/")


async def commit_initial_preview(request: web.Request) -> web.Response:
    runtime: Runtime = request.app["runtime"]
    _require_admin(request, runtime)
    if runtime.service is None:
        raise web.HTTPServiceUnavailable(text="Gmail connection is waiting for authorization")
    if runtime.pending_preview is None:
        raise web.HTTPConflict(text="run and review the bounded preview first")
    body = await request.json()
    if body.get("next_history_id") != runtime.pending_preview.next_history_id:
        raise web.HTTPConflict(text="preview revision does not match")
    imported = await runtime.service.commit_preview(runtime.pending_preview)
    runtime.pending_preview = None
    runtime.initial_preview_required = False
    runtime.sync_task = asyncio.create_task(runtime._sync_loop())
    return web.json_response({"status": "committed", "imported": imported})


def _public_reply(delivery_id: str, payload: dict[str, Any]) -> PublicReply:
    return PublicReply(
        delivery_id=delivery_id,
        chatwoot_message_id=int(payload["id"]),
        conversation_id=int(payload["conversation"]["id"]),
        account_id=int(payload["account"]["id"]),
        inbox_id=int(payload["inbox"]["id"]),
        sender_id=int(payload["sender"]["id"]),
        content=str(payload.get("content", "")),
        content_type=str(payload.get("content_type", "text")),
        private=bool(payload.get("private", False)),
        message_type=str(payload.get("message_type", "")),
        event=str(payload.get("event", "")),
        attachments=tuple(payload.get("attachments") or ()),
    )


async def chatwoot_proxy(request: web.Request) -> web.StreamResponse:
    runtime: Runtime = request.app["runtime"]
    target = runtime.config.chatwoot_url + request.rel_url.path_qs
    if request.headers.get("Upgrade", "").lower() == "websocket":
        return await _websocket_proxy(request, target)
    excluded = {"host", "content-length", "connection", "transfer-encoding"}
    headers = {key: value for key, value in request.headers.items() if key.lower() not in excluded}
    body = await request.read()
    timeout = aiohttp.ClientTimeout(total=120)
    async with aiohttp.ClientSession(timeout=timeout) as session:
        async with session.request(
            request.method, target, headers=headers, data=body, allow_redirects=False
        ) as upstream:
            response_headers: CIMultiDict[str] = CIMultiDict()
            for key, value in upstream.headers.items():
                if key.lower() not in {"content-length", "connection", "transfer-encoding"}:
                    response_headers.add(key, value)
            location = response_headers.get("Location")
            if location and location.startswith(runtime.config.chatwoot_url):
                response_headers["Location"] = (
                    runtime.config.app_origin
                    + location.removeprefix(runtime.config.chatwoot_url)
                )
            return web.Response(
                status=upstream.status,
                headers=response_headers,
                body=await upstream.read(),
            )


async def _websocket_proxy(request: web.Request, target: str) -> web.WebSocketResponse:
    client = web.WebSocketResponse()
    await client.prepare(request)
    session = aiohttp.ClientSession()
    try:
        headers = {
            key: value
            for key, value in request.headers.items()
            if key.lower()
            not in {
                "host",
                "connection",
                "upgrade",
                "sec-websocket-key",
                "sec-websocket-version",
                "sec-websocket-extensions",
            }
        }
        async with session.ws_connect(target, headers=headers) as upstream:
            async def forward(source: Any, destination: Any) -> None:
                async for message in source:
                    if message.type == WSMsgType.TEXT:
                        await destination.send_str(message.data)
                    elif message.type == WSMsgType.BINARY:
                        await destination.send_bytes(message.data)
                    elif message.type in {WSMsgType.CLOSE, WSMsgType.CLOSED, WSMsgType.ERROR}:
                        break

            left = asyncio.create_task(forward(client, upstream))
            right = asyncio.create_task(forward(upstream, client))
            done, pending = await asyncio.wait({left, right}, return_when=asyncio.FIRST_COMPLETED)
            for task in pending:
                task.cancel()
            for task in done:
                task.result()
    finally:
        await session.close()
    return client


def main() -> None:
    logging.basicConfig(level="INFO", format="%(asctime)s %(levelname)s %(name)s %(message)s")
    config = Config.from_environment()
    web.run_app(create_application(Runtime(config)), host=config.listen_host, port=config.listen_port)


if __name__ == "__main__":
    main()
