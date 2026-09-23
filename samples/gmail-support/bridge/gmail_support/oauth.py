from __future__ import annotations

import asyncio
import base64
import hashlib
import json
import secrets
import time
from dataclasses import dataclass

import aiohttp
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from google_auth_oauthlib.flow import Flow

from .config import Config
from .store import Store


SCOPES = [
    "https://www.googleapis.com/auth/gmail.readonly",
    "https://www.googleapis.com/auth/gmail.send",
]


@dataclass(frozen=True)
class OAuthStart:
    authorization_url: str
    state: str


class OAuthCoordinator:
    def __init__(self, config: Config, store: Store) -> None:
        self.config = config
        self.store = store
        self._update_token = config.refreshable_gmail_binding.update_token

    def start(self) -> OAuthStart:
        state = secrets.token_urlsafe(32)
        verifier = secrets.token_urlsafe(64)
        challenge = base64.urlsafe_b64encode(
            hashlib.sha256(verifier.encode()).digest()
        ).decode().rstrip("=")
        flow = self._flow(state=state, verifier=verifier)
        authorization_parameters = {
            "access_type": "offline",
            "include_granted_scopes": "true",
            "prompt": "consent",
            "code_challenge": challenge,
            "code_challenge_method": "S256",
        }
        if self.config.google_oauth_client.get("login_hint"):
            authorization_parameters["login_hint"] = self.config.google_oauth_client[
                "login_hint"
            ]
        url, returned_state = flow.authorization_url(
            **authorization_parameters,
        )
        if returned_state != state:
            raise RuntimeError("Google OAuth state changed unexpectedly")
        expires_at = int(time.time()) + 10 * 60
        self.store.create_oauth_state(
            state=state,
            operation_user_id=self.config.oauth_operator_user_id,
            ato_account_id=self.config.ato_account_id,
            instance_id=self.config.instance_id,
            binding_id="gmail_oauth",
            expected_mailbox=self.config.support_address,
            encrypted_verifier=self._encrypt_verifier(state, verifier),
            expires_at=expires_at,
        )
        return OAuthStart(url, state)

    async def exchange(self, *, state: str, code: str) -> str:
        record = self.store.consume_oauth_state(state, int(time.time()))
        if (
            record["operation_user_id"] != self.config.oauth_operator_user_id
            or record["ato_account_id"] != self.config.ato_account_id
            or record["instance_id"] != self.config.instance_id
            or record["binding_id"] != "gmail_oauth"
            or record["expected_mailbox"] != self.config.support_address
        ):
            raise ValueError("OAuth state does not belong to this connection")
        verifier = self._decrypt_verifier(state, str(record["pkce_verifier_encrypted"]))
        flow = self._flow(state=state, verifier=verifier)
        await asyncio.to_thread(flow.fetch_token, code=code)
        credentials = flow.credentials
        if not credentials.refresh_token:
            raise ValueError("Google did not return an offline refresh token")
        granted = frozenset(credentials.granted_scopes or credentials.scopes or ())
        if not set(SCOPES).issubset(granted):
            raise ValueError("Google did not grant both required Gmail scopes")
        value = json.dumps(
            {
                "client_id": credentials.client_id,
                "client_secret": credentials.client_secret,
                "refresh_token": credentials.refresh_token,
                "token_uri": credentials.token_uri,
                "scopes": sorted(granted),
            },
            separators=(",", ":"),
            sort_keys=True,
        )
        return value

    async def persist_binding(self, value: str) -> None:
        async with aiohttp.ClientSession() as session:
            async with session.put(
                self.config.refreshable_gmail_binding.update_url,
                headers={
                    "Authorization": f"Bearer {self._update_token}",
                    "Content-Type": "application/json",
                },
                json={"value": value},
                proxy="http://127.0.0.1:18080",
            ) as response:
                payload = await response.json(content_type=None)
                if response.status != 200 or not isinstance(payload.get("next_token"), str):
                    raise RuntimeError(f"Ato rejected the Gmail Binding update ({response.status})")
                self._update_token = payload["next_token"]

    def _flow(self, *, state: str, verifier: str) -> Flow:
        client = {
            key: value
            for key, value in self.config.google_oauth_client.items()
            if key not in {"redirect_uri", "login_hint"}
        }
        client["redirect_uris"] = [self.config.google_oauth_client["redirect_uri"]]
        return Flow.from_client_config(
            {"web": client},
            scopes=SCOPES,
            state=state,
            code_verifier=verifier,
            redirect_uri=self.config.google_oauth_client["redirect_uri"],
        )

    def _encrypt_verifier(self, state: str, verifier: str) -> str:
        nonce = secrets.token_bytes(12)
        encrypted = AESGCM(self.config.oauth_state_key).encrypt(
            nonce, verifier.encode(), state.encode()
        )
        return base64.urlsafe_b64encode(nonce + encrypted).decode().rstrip("=")

    def _decrypt_verifier(self, state: str, value: str) -> str:
        decoded = base64.urlsafe_b64decode(value + "=" * (-len(value) % 4))
        return AESGCM(self.config.oauth_state_key).decrypt(
            decoded[:12], decoded[12:], state.encode()
        ).decode()
