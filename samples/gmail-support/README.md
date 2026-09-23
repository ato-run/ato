# Ato Support Desk

This private portable Application reuses Chatwoot Community Edition for the
support inbox, conversations, notes, assignment, labels, status and canned
responses. A small Bridge owns only the Google-specific boundary:

```text
Gmail / Google Workspace
        ⇅ Gmail API over restricted HTTPS
Gmail API Bridge
        ⇅ signed API Channel webhook + Chatwoot account API
Chatwoot CE
        ⇅ one authenticated Ato Web Surface
```

It does not run SMTP, change MX/PTR/forwarding, replace Chatwoot's ticket UI,
or use Chatwoot's standard Google channel. That upstream channel authenticates
to `imap.gmail.com`; this sample uses the Gmail API through an API Channel.

The sample is currently **connection waiting**. The local mock and real
Chatwoot boundary have been accepted, but no real Google mailbox has been
authorized and no production switch is approved.

## Pinned components

| Component | Pin | License boundary |
| --- | --- | --- |
| Chatwoot CE | `v4.18.0`, upstream amd64 manifest `sha256:03a03a85a00f1d119367deb0d090a56e553468d0aa5e9194a57eba7c61deb7de` | MIT; `/app/enterprise` is removed from the derived image |
| Support image | `ato.local/gmail-support:0.1.0@sha256:f270669fe83479ceebbdea567b08d9b8c4821d41ea2447742933558bf68860dd` | locally built fixture; not yet published to a registry |
| Redis | `8.2.1-alpine@sha256:f887e6dacdcfa8e14af2f625fdf4474ff8c37dc36ce13b9e89e6e9a901f155ad` | BSD-3-Clause |
| PostgreSQL | Alpine `17.11-r0` in the Support image | PostgreSQL |
| pgvector | Alpine `0.6.2-r1` in the Support image | PostgreSQL |

See [THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md) for the Bridge's direct
dependencies. Paid Chatwoot modules are not installed or enabled.

## Runtime shape

The current portable OCI service-group profile exposes one HTTP service and
permits one filesystem state attachment. The `support` container therefore
runs the official Chatwoot Web and Sidekiq processes, PostgreSQL 17, and the
Bridge as distinct processes in one container. Redis remains a separate,
ephemeral service. This is a packaging consequence of the current Ato state
boundary, not an attempt to turn Chatwoot into a single process.

All application processes run as uid/gid 1000. PID 1 starts as root only to
repair ownership after Ato restores a uid-normalized filesystem archive, then
drops privileges with `su-exec`. The one state tree contains PostgreSQL,
Chatwoot Active Storage, and the Bridge's SQLite mapping/outbox database.

The manifest requests 3.75 GiB/3750m/896 pids for `support` and
256 MiB/250m/128 pids for Redis: exactly the current 4 GiB, 4 CPU and 1024 pid
service-group ceilings.

## Bindings

The Capsule never contains an OAuth refresh token, Google client secret,
Chatwoot bootstrap password, or Ato update capability.

| Binding | Protocol | Required | Purpose |
| --- | --- | --- | --- |
| `chatwoot_secret_key` | `ato.secret@1` | yes | Chatwoot Rails secret key base |
| `chatwoot_runtime` | `ato.secret@1` | yes | bootstrap/admin and OAuth state configuration |
| `app_origin` | `ato.config@1` | yes | stable external Instance origin |
| `gmail_oauth` | `ato.secret-refresh@1` | no | encrypted refresh credential plus exact-Run update capability |
| `google_oauth_client` | `ato.secret@1` | yes | dedicated Google OAuth client configuration |
| `google_https_egress` | `ato.tcp-egress@1` | no | Ato SOCKS route to one operator-approved HTTPS proxy |

`chatwoot_runtime` is a secret JSON object with these keys:

```json
{
  "bootstrap_admin_email": "operator@example.invalid",
  "bootstrap_admin_name": "Support operator",
  "bootstrap_admin_password": "<generated>",
  "bridge_admin_token": "<generated>",
  "ato_account_id": "<account id>",
  "instance_id": "<instance id>",
  "oauth_operator_user_id": "<user id>",
  "oauth_state_key": "<base64url 32-byte key>",
  "https_proxy_target": "192.0.2.10:443",
  "https_proxy_authorization": "Basic <optional>"
}
```

`google_oauth_client` is a Google Web application client object. Its
`redirect_uri` must be exactly
`https://<instance-host>/__ato/gmail/oauth/callback`; an optional `login_hint`
may name the expected Workspace user.

The Bridge accepts HTTPS `CONNECT` only for `oauth2.googleapis.com`,
`gmail.googleapis.com`, and the exact hostname of its Ato Binding-update URL.
It first connects to the numeric, operator-configured upstream proxy through
the separately granted Ato SOCKS Binding. Browser navigation to
`accounts.google.com` does not pass through the workload. Direct unrestricted
egress, `0.0.0.0/0`, and disabled TLS verification are not supported.

## Google OAuth setup

1. Create a dedicated Google Cloud project and Web OAuth client. Do not reuse
   Ato login credentials or a ChatGPT Gmail connection.
2. For one-company Workspace use, prefer an **Internal** consent screen. An
   External app in Testing is not an operational endpoint: Gmail refresh
   tokens expire after seven days in that mode.
3. Enable the Gmail API and configure only
   `https://www.googleapis.com/auth/gmail.readonly` and
   `https://www.googleapis.com/auth/gmail.send`.
4. Supply the exact redirect URI and bind the client JSON as
   `google_oauth_client`.
5. An authenticated operator calls `POST /__ato/gmail/oauth/start` with the
   Bridge admin bearer token and opens the returned authorization URL. The
   one-time state is bound to the operation user, Ato account, Instance,
   Binding and expected mailbox, and the PKCE verifier is encrypted at rest.
6. The callback calls Gmail `users.getProfile` and
   `users.settings.sendAs.list`. `support@ato.run` must be the primary address
   or an accepted send-as alias before the encrypted refresh Binding is
   updated. A forwarding address alone never authorizes a forged From header.
7. Call `GET /__ato/gmail/preview`, review the bounded result, then commit the
   exact returned history revision with `POST /__ato/gmail/preview/commit`.

The admin token is for an operator-side command, not browser storage. The
OAuth callback itself is authorized by the random, one-use, ten-minute state.

## Synchronization and send semantics

- Initial import is bounded to 100 messages and the configured 30-day query.
  It is previewed before storage. No import marks read, archives, deletes,
  labels, replies, or drafts a message.
- Incremental sync uses `history.list`. An expired history cursor (404) uses
  the same bounded query and deduplication before advancing the cursor.
- `(mailbox_id, Gmail thread ID)` maps to one Chatwoot conversation;
  `(mailbox_id, Gmail message ID)` is the deduplication key. Different Gmail
  threads are never merged because their senders match.
- A support label can be configured for a shared mailbox. The Bridge filters
  both Gmail retrieval and local storage by the label/delivery headers and
  existing thread map. This is data minimization, not a narrower OAuth grant.
- Remote HTML is reduced to a small safe subset. Images, scripts, styles and
  tracking fetches are removed. Network isolation also prevents Chatwoot jobs
  from fetching public avatars or previews.
- Each received attachment is limited to 5 MiB, each decoded message to 25
  MiB, and a message to 32 attachments. Reply attachments have a 5 MiB
  aggregate limit. Large bodies are never placed in an Operation or MCP
  response.
- Only a signed `message_created` event for a public outgoing reply from the
  configured account, inbox, conversation and allowed human agent may send.
  Internal notes, incoming messages, system activity, and imported Gmail sent
  messages are ignored.
- Replies preserve Gmail `threadId`, the original subject, `In-Reply-To` and
  `References`. The current version sends to the stored normal reply target.
  It does not expose CC editing or Reply All.
- The outbox is durable before Gmail is called. A repeated webhook, click, or
  restart reuses the same record. Timeout or ambiguous API failure becomes
  `outcome_unknown`, marks the Chatwoot message visibly failed, and is never
  retried automatically. A later Sent-mail observation reconciles it only
  when the outbox marker, RFC Message-ID and Gmail thread all agree.
- `accepted_by_gmail_api` means Gmail accepted the request. It is not evidence
  of delivery to the recipient's mailbox.

## Authentication and private access

The only public Surface proxies to Chatwoot. Chatwoot authentication remains
enabled, account signup is disabled, and the API inbox webhook is restricted
to an exact signed loopback URL. The derived image does not enable Chatwoot's
global private-network webhook escape hatch. Direct routes, APIs and
attachments still require Chatwoot/Ato authorization; hiding the app in My
Apps is not treated as an access control.

## Stop, checkpoint and restore

Stopping the service cancels Gmail polling and terminates Web, Sidekiq, Bridge
and PostgreSQL before the state is packed. A Bridge restart changes an in-flight
`sending` outbox row to `outcome_unknown`; it never resends it. A restored
Instance remains behind Ato's existing restore reapproval hold. After release,
the operator must compare unknown outbox rows with Gmail Sent mail before any
manual retry. Restoring local state cannot retract mail already sent outside
Ato.

The current `ato.state.filesystem@1` artifact is an uncompressed canonical tar
with a 64 MiB limit. The stopped acceptance state was 52,787,200 bytes as an
ordinary tar; this is below the limit but leaves only about 11 MiB for mail and
attachments.
This is a measured staging limitation, not production capacity. Do not approve
production until the generic state transport supports larger/chunked state or
PostgreSQL and attachments move to an approved durable service. Before every
checkpoint, stop the Instance and verify the reported artifact size is below
64 MiB. A failed/oversized checkpoint is not a backup.

## Cutover and rollback

1. Confirm the real mailbox identity and accepted `support@ato.run` send-as.
2. Run the bounded preview and test only the named sender/recipient.
3. Verify receive, reply/reply, Japanese text, safe HTML, a small attachment,
   restart, duplicate webhook, OAuth revoke, checkpoint/restore and UI access.
4. Stop any legacy automatic reply or sender before enabling this Bridge.
5. Keep ordinary Gmail available. Stopping the Bridge returns operations to
   manual Gmail without changing MX or mailbox delivery.
6. Production cutover requires a separate acceptance report and approval.

## MCP status

The existing `ato.operations/v1` executor only performs isolated synchronous
JavaScript state transforms with `effects.external = none`; it cannot call a
running Chatwoot API or authorize an email send. Adding fake local ticket state
would violate the shared-business-service boundary. The intended catalog names
are `support_list_conversations`, `support_get_conversation`,
`support_add_internal_note`, `support_assign_conversation`,
`support_update_status`, `support_create_reply_draft`, and
`support_send_approved_reply`, but they remain blocked on a generic
external-effect/headless Operation Adapter. Send approval must bind recipient,
body, attachment digests and draft revision. UI support is implemented; MCP
acceptance is not.

## Local verification

```sh
cd samples/gmail-support
PYTHONPATH=bridge python3 -m unittest discover -s bridge/tests -v
python3 -m compileall -q bridge/gmail_support
ruby -c support/bootstrap_chatwoot.rb
ruby -c support/ato_loopback_webhook.rb
sh -n support/supervise.sh
docker build --platform linux/amd64 -f Dockerfile.support \
  -t ato.local/gmail-support:0.1.0 .
```

From the repository root, pack with the workspace CLI:

```sh
cargo run -p ato-cli --bin ato -- pack samples/gmail-support \
  --output .tmp/gmail-support.capsule
```
