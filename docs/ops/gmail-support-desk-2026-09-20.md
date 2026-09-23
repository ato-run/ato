# Gmail API Support Desk acceptance — 2026-09-20

## Result

Chatwoot CE plus the Gmail API Bridge is implemented on the existing Draft
stack. The fixture is not connected to Google and is not approved for
production. Its current state is **connection waiting**.

No public SMTP listener, MX/PTR change, public Discover entry, Gmail delivery,
or production cutover was performed. The existing Stalwart/Missive fixture and
its acceptance record remain unchanged.

## Implemented boundary

- Chatwoot CE supplies the inbox, conversation UI, internal notes, assignment,
  labels, status and canned responses.
- The Bridge supplies OAuth state/PKCE, Gmail profile/send-as admission,
  bounded preview, `history.list` synchronization, Gmail/Chatwoot identity
  maps, signed webhook admission, MIME replies and the durable outbox.
- `ato.secret-refresh@1` in API #664 persists a post-launch refresh credential
  only inside the existing Instance/Contract-bound encrypted Binding map. Its
  update capability is scoped to the exact Instance, Run, lease, Binding and
  configuration generation and is blocked by stop and restore hold.
- HTTPS egress remains separately authorized. The workload's local CONNECT
  gate accepts only Google OAuth/Gmail and the exact Ato update hostname, then
  uses an operator-approved numeric upstream proxy through Ato SOCKS.

## Immutable inputs

| Field | Value |
| --- | --- |
| Chatwoot | `v4.18.0` |
| upstream amd64 manifest | `sha256:03a03a85a00f1d119367deb0d090a56e553468d0aa5e9194a57eba7c61deb7de` |
| derived support image | `ato.local/gmail-support:0.1.0@sha256:f270669fe83479ceebbdea567b08d9b8c4821d41ea2447742933558bf68860dd` |
| Redis | `docker.io/library/redis:8.2.1-alpine@sha256:f887e6dacdcfa8e14af2f625fdf4474ff8c37dc36ce13b9e89e6e9a901f155ad` |
| bundle | `sha256:65c68f0d179e2447f416317f1f82637cdc5c7fe64074fc82875a44988180c0f7` (189,218 bytes) |
| `ContractRef` | `sha256:a411f8e34e3d57e39f902b36d0fd0a7b70706d3da2954a96b45cc07abcab9e7c` |
| `DerivationRef` | `sha256:b9dda9a44ffacf6436613e795caa3c34f2f4fd23149fec7005137451333784df` |

The derived image removes `/app/enterprise`. It is local-only until an approved
registry and Runner preload path are selected.

## Local evidence

- 11 Bridge unit tests pass: bounded filtering/deduplication, history expiry,
  note suppression, threading, revision fencing, ambiguous-send hold,
  Sent-mail reconciliation, permanent failure visibility, raw-body webhook
  signature verification, HTML image/script removal and attachment-origin
  pinning.
- A real Chatwoot 4.18 container reached
  `status=connection_waiting`; the UI proxy returned 200 and rewrote its login
  redirect to the external app origin.
- Re-importing the same Gmail thread and message through the real Chatwoot API
  reused one conversation and left exactly one imported message, including the
  crash window before the local mapping commit.
- Public account creation returned 404 with `ENABLE_ACCOUNT_SIGNUP=false`.
- The derived image contains no `/app/enterprise`; the `ato` user is uid/gid
  1000 and application processes run under that user.
- An actual Chatwoot API Channel internal note remained private and did not
  create an outbox row. A public reply while OAuth was absent became failed
  and did not create an outbox row or call Gmail.
- The same stopped-send webhook returned 503 once and was then recognized as
  consumed on redelivery, so OAuth recovery cannot release an old reply.
- PostgreSQL, the Chatwoot conversation and Bridge delivery records survived
  a graceful stop/restart using the same state tree.
- The stopped state occupied 48,157,978 logical file bytes; after the
  persistence restart and duplicate-import acceptance its ordinary tar was
  52,787,200 bytes, below the 64 MiB
  Ato state-artifact ceiling.
- API typecheck and 22 focused tests passed, including encrypted Binding
  update/rotation, replay refusal, stop refusal, restore-hold refusal and the
  existing single-flight wake suite.

## Detected mismatches and remaining gates

1. **Google connection:** `support@ato.run` has not been proven to be an
   independent mailbox or accepted send-as alias. A dedicated OAuth client,
   upstream restricted proxy and the operator's consent are absent. The app
   URL and Instance ID are therefore not allocated yet.
2. **Hosted acceptance:** no staging import, Runner preload, real Gmail
   receive/reply, Japanese HTML/attachment, OAuth revoke or hosted restore has
   run. Mock success is not reported as real-mail acceptance.
3. **State capacity:** the clean state is already about 49 MiB versus a 64 MiB
   uncompressed archive limit. This can support bounded staging evidence but
   is not safe production capacity. A generic larger/chunked state transport
   or approved external durable PostgreSQL/object storage is required before
   production approval.
4. **MCP:** current `ato.operations/v1` forbids external effects and cannot
   reach the live Chatwoot API. The UI and Bridge are implemented, but the
   requested common-service MCP Adapter needs a generic external-effect and
   approval-capable Operation runtime. No second ticket database or
   email-specific permission bypass was introduced.

## OAuth and staged acceptance checklist

1. Create a dedicated Workspace Internal OAuth application when applicable,
   with only Gmail readonly/send scopes and the exact Instance callback.
2. Confirm `users.getProfile` and accepted `users.settings.sendAs.list` output
   for `support@ato.run`; otherwise stop and provide the required Workspace
   administrator change.
3. Import the exact bundle as a Private Instance, bind generated Chatwoot
   secrets and the restricted HTTPS proxy, and verify Ato plus Chatwoot auth on
   UI/API/attachment routes.
4. Review and commit the bounded 30-day/100-message preview using only the
   approved test sender and recipient.
5. Exercise receive, reply/reply, note suppression, Japanese, safe HTML, a
   small attachment, duplicate delivery/click/restart, outcome unknown,
   manual Gmail sent-mail import, OAuth revoke and stop.
6. Checkpoint while the artifact remains below 64 MiB, mutate, restore, prove
   the restore hold prevents startup/update, release it, and reconcile outbox
   against Gmail Sent before allowing a send.
7. Present the resulting URL, Instance ID, receipt, artifact size and mail
   evidence for approval. Only then schedule a production cutover that first
   stops the old automatic sender; stopping the Bridge remains the rollback.
