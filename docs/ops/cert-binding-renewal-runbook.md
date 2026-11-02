# Certificate Binding renewal — operator runbook (draft, 2026-11-02)

Status: Draft. Nothing in this runbook has been installed or executed on any
host. It describes the S2 procedure of the self-hosted mail plan
(ato-run/ato#1665); the design is ADR-044
(`docs/rfcs/draft/ADR-044-certificate-binding-automatic-renewal.md`) and the
local tool is `tools/cert-export`. Language: en.

The aim: Caddy renews, nothing else is done by hand, and every renewal ends with
a new Run that serves the new certificate, with the serial compared against the
export record as the proof.

## 0. Preconditions

Do not start this procedure until all of the following are true:

1. `ato` has the two-Binding path working for an OCI service group — today the
   OCI adapter refuses a Binding value containing a newline, so a PEM Binding
   fails at admission (ADR-044, "Detected blocker").
2. `ato-api` has `GET`/`POST /v1/operator/certificate-bindings/:grantId`
   (ADR-044, "Decision 2"), deployed to the environment in use.
3. S1 (`ato.contract.tls@1`, receipt `/3`) is merged, so the reflection proof
   exists.
4. The Instance is imported, running, and its Stalwart configuration references
   `ATO_BINDING_MAIL_CERT_PEM` / `ATO_BINDING_MAIL_KEY_PEM`.

If (1) is not true, the honest interim is the current host-side copy into
Stalwart with a manual restart, and this runbook does not apply.

## 1. Allowlist

One root-owned file, `0600`, root-owned. The tool refuses to run otherwise.

```text
# /etc/ato/cert-export/allowlist.conf
allowlist = "ato.cert-export.allowlist/1"
# instance_id        domain            certificate                    private key                    cert-binding     key-binding
i_01J8ZQ0K9R3T6V      mail.ato.run      /srv/caddy-data/mail.ato.run.crt  /srv/caddy-data/mail.ato.run.key  mail_cert_pem  mail_key_pem
```

Rules: one line per (Instance, domain); absolute paths; binding ids exactly as
the Capsule declares them; the private key file must not be group/world
readable. The tool never reads anything outside the selected line.

Validate before installing anything:

```sh
install -o root -g root -m 0600 allowlist.conf /etc/ato/cert-export/allowlist.conf
ato-cert-export --instance i_01J8ZQ0K9R3T6V check   # exit 0 and a plan line
```

`check` writes nothing. It is the safe way to see what the timer will see.

## 2. Credential

- Issued once by an admin session on the operator surface, as a grant for one
  (Instance, domain) and the two binding ids. The API returns the token **once**.
- Stored in `/etc/ato/cert-export/operator.token`, `0400 root:root`, outside the
  repository and outside the export directory.
- Rotated on every successful apply: after each renewal the API returns the next
  token and the same script rewrites the file with the new value.
- Revoked by an admin session (`revoked_at`); the next apply fails `403`.
- Never printed: not by the tool (it reads the file itself), not by `set -x`,
  not by `curl -v`, not in a journal line.

```sh
install -o root -g root -m 0400 /dev/null /etc/ato/cert-export/operator.token
# paste the one-time token with an editor, never on the command line
```

## 3. State and export directories

```text
/var/lib/ato/cert-export/                     0700 root:root
/var/lib/ato/cert-export/<instance>/          0700
    cert.pem  key.pem (0600)  meta.json (0600)
    *.prev                                     kept across a renewal
```

The export directory is the only thing the tool writes. `meta.json` is the
record ops compares against; it holds no private key material. `*.prev` is the
previous validated pair and is the rollback source.

## 4. Timer and service

`/etc/systemd/system/ato-cert-export.service`:

```ini
[Unit]
Description=ato certificate Binding export and renewal (one Instance)
After=network-online.target caddy.service
Wants=network-online.target

[Service]
Type=oneshot
User=root
ExecStart=/usr/local/libexec/ato-cert-renew.sh
# The script never writes key material anywhere except the export directory.
PrivateTmp=true
NoNewPrivileges=true
ProtectHome=true
ProtectSystem=strict
ReadWritePaths=/var/lib/ato/cert-export
```

`/etc/systemd/system/ato-cert-export.timer`:

```ini
[Unit]
Description=Hourly ato certificate Binding export

[Timer]
OnCalendar=hourly
RandomizedDelaySec=300
Persistent=true
Unit=ato-cert-export.service

[Install]
WantedBy=timers.target
```

`/usr/local/libexec/ato-cert-renew.sh` (`0700 root:root`). It reads the current
generation, exports, and pipes the payload straight into the updater — stdout of
`export --print-binding-update` is the payload alone:

```sh
#!/bin/sh
set -eu
umask 077

api="${ATO_API_ORIGIN:-https://api.ato.run}"
grant="${ATO_CERT_BINDING_GRANT:?}"
instance="${ATO_CERT_BINDING_INSTANCE:?}"
token_file=/etc/ato/cert-export/operator.token
export_dir=/var/lib/ato/cert-export/"$instance"

token=$(cat "$token_file")
run_dir=/var/lib/ato/cert-export/"$instance"
state=$(curl -fsS -H "Authorization: Bearer $token" \
  "$api/v1/operator/certificate-bindings/$grant")
generation=$(printf '%s' "$state" | jq -r '.generation')

# Plan and warnings go to the journal; the payload goes to a private file so
# the action can be checked before anything is sent. Never run this
# unconditionally: an unchanged pair must NOT restart the mail Run.
/usr/local/bin/ato-cert-export \
  --allowlist /etc/ato/cert-export/allowlist.conf \
  --instance "$instance" \
  --openssl /usr/bin/openssl \
  export --print-binding-update \
  --grant-id "$grant" --expected-generation "$generation" \
  > "$run_dir/.payload" 2> "$run_dir/.plan"
if grep -q '"action":"unchanged"' "$run_dir/.plan"; then
  cat "$run_dir/.plan"
  rm -f "$run_dir/.payload" "$run_dir/.plan"
  exit 0
fi
cat "$run_dir/.plan"
curl -fsS -X POST \
  -H "Authorization: Bearer $token" \
  -H "Content-Type: application/json" \
  --data-binary @"$run_dir/.payload" \
  "$api/v1/operator/certificate-bindings/$grant/apply" \
  > "$run_dir/last-apply.json"
rm -f "$run_dir/.payload" "$run_dir/.plan"

# The response carries the rotated token; store it, never print it.
jq -r '.next_token' /var/lib/ato/cert-export/"$instance"/last-apply.json \
  > "$token_file.new" && chmod 0400 "$token_file.new" && mv "$token_file.new" "$token_file"
rm -f /var/lib/ato/cert-export/"$instance"/last-apply.json
```

Notes on the script:

- `export` with `--print-binding-update` sends the plan JSON to **stderr** and
  the payload to stdout. The script sends them to two private files first, so it
  can read `action` and skip the call.
- If the pair is unchanged the tool is a no-op on disk, **and the apply call is
  skipped**. Applying an unchanged pair is not harmless: it starts a new Run and
  briefly drops the mail ports, so the `unchanged` branch is a hard requirement,
  not a preference.
- `last-apply.json` is the only file that ever holds the rotated token; it is
  removed in the same run. If a run dies between the response and the `mv`, the
  old token is still valid until the next successful rotation and the next
  renewal recovers by... nothing: rotate the grant manually in that case
  (§8).
- The script is short on purpose: every line that touches key or token material
  is visible here and nowhere else.

## 5. What a renewal looks like

1. Caddy renews in its own storage (never shared, never mounted).
2. The timer runs: `GET` status → `export` (validate; write only if the source
   bytes changed) → `POST apply`.
3. The API verifies the grant, the two binding ids, the domain, the value shape
   and `expected_generation`, seals the new values, writes one audit row, and
   replaces the Run.
4. The old Run is stopped first (fixed TCP cannot be served by two Runs), then
   the new Run starts with the new pair. Consequence: a stop failure leaves the
   old valid certificate serving, but **a stop success followed by a start
   failure leaves the mail ports down** until the start succeeds. That is the
   only externally visible failure of a renewal and it is alerted as critical.
5. The K5 observation records the presented serial in receipt `/3`.
6. Ops compares (§7).

## 6. Failure behaviour

| Failure | What you see | What to do |
|---|---|---|
| Source pair unchanged | `action: unchanged` in the journal, no `*.prev` | Nothing. |
| Validation rejected (exit `2`) | `validation: …` on stderr; the export directory is untouched | Fix the cause (key/cert mismatch, SAN, expiry, clock). The running Run is still serving the old, still-valid pair. |
| Allowlist/usage problem (exit `3`) | `usage: …` | Fix the file or the arguments; no secret was touched. |
| Not root (exit `4`) | `privilege: …` | Run through the unit; `--allow-unprivileged` is for tests only. |
| openssl or export directory failure (exit `5`) | `tool:`/`io:` | Check `/usr/bin/openssl` ownership and the export directory; the running Run is unaffected. |
| Apply rejected | curl exit `22`, the body in the journal | `401` missing/malformed token: check the token file. `403` revoked grant. `410` expired/rotated/consumed grant: re-issue (§8). `409` generation conflict: the next tick wins. `413/422` value shape or size: fix and retry. Never disable the check. |
| Apply `502`, `stopped: false`, `run_id: null` | The CAS committed but the stop failed; the old Run still serves the old valid pair | Alert. Retry the replace; the next wake uses the new pair. |
| Apply `502`, `stopped: true`, `run_id: null` | **Mail is down**: the old Run was stopped, the new one did not start | Critical alert. Retry the start; if it cannot become ready, roll back (§9). Do not wait for the next tick. |
| `notAfter < 14 days` warning | `warning: the certificate expires in N days` | Caddy is not renewing: fix DNS/ACME, then run the unit by hand. |
| Serial mismatch after a renewal | K5 receipt serial ≠ `meta.json` serial | Incident: the new Run did not pick up the values, or something else answers on the port. Roll back (§9) and investigate. |

## 7. Verifying a renewal end to end

Three independent reads must agree:

```sh
# 1. What this host exported and validated
jq -r '.certificate.serial, .certificate.not_after' \
  /var/lib/ato/cert-export/<instance>/meta.json | tr 'A-F' 'a-f'

# 2. What the API recorded for the run (owner session required)
curl -fsS -H "Authorization: Bearer <owner session>" \
  "$api/v1/compute-instances/<instance>/runtime-evidence" | jq

# 3. What a public port actually presents (outside the host)
openssl s_client -connect mail.ato.run:993 -servername mail.ato.run </dev/null 2>/dev/null \
  | openssl x509 -noout -serial -enddate
```

Comparison rules:

- `serial` from (1), (2)'s receipt `/3` TLS evidence for every port, and (3) on
  every port (25 via STARTTLS, 465, 587, 993, and 443) must be the same
  lowercase hex string.
- `not_after` must match `meta.json`.
- The chain must reach a public root; the K5 observation already requires it.
- Record the four values (serial, not_after, export time, apply time) in the
  renewal record. This comparison — not the exit code of the timer — is the
  acceptance evidence for S2.

STARTTLS port:

```sh
openssl s_client -starttls smtp -connect mail.ato.run:587 -servername mail.ato.run </dev/null 2>/dev/null \
  | openssl x509 -noout -serial
```

## 8. Rotate or re-issue the credential

- Rotation happens automatically on every successful apply.
- To rotate deliberately, or after a suspicious event: revoke the grant from the
  operator surface, issue a new one for the same (Instance, domain) and binding
  ids, write the one-time token with an editor to the `0400` file, and run the
  unit by hand once. The tool is idempotent, so a manual run is safe.
- `410` from the apply endpoint means the grant expired or its
  `config_generation` is stale: re-issue.
- Do not shorten the check by skipping `GET`: `expected_generation` is what
  makes a second timer or a retry unable to overwrite a newer pair.

## 9. Rollback

A renewal can end in three states: nothing changed (checks/CAS failed before any
stop), the durable config is ahead but the old Run still serves (stop failed), or
the old Run is stopped and the new one is not running (start failed). Rollback
covers the last two: put the previous pair back, then start the Run.

1. Take the pair from the export's `*.prev` (root-only, next to the current
   pair). Do not copy it anywhere else — the private key stays under
   `/var/lib/ato/cert-export/`.
2. Create a root-only rollback allowlist that names the same (Instance, domain)
   and the two binding ids, with the `.prev` paths as sources:

   ```text
   allowlist = "ato.cert-export.allowlist/1"
   i_01J8ZQ0K9R3T6V  mail.ato.run  /var/lib/ato/cert-export/<instance>/cert.pem.prev  /var/lib/ato/cert-export/<instance>/key.pem.prev  mail_cert_pem  mail_key_pem
   ```

   `chmod 0600`, root-owned.
3. Export into a separate directory so the current pair is not overwritten:

   ```sh
   ato-cert-export --allowlist /etc/ato/cert-export/allowlist.rollback.conf \
     --instance <instance> --export-dir /var/lib/ato/cert-export/<instance>-rollback \
     export --print-binding-update --grant-id <grant> --expected-generation <generation> \
     | curl -fsS -X POST -H "Authorization: Bearer $(cat /etc/ato/cert-export/operator.token)" \
         -H "Content-Type: application/json" --data-binary @- \
         "$api/v1/operator/certificate-bindings/<grant>/apply"
   ```
4. The tool re-validates `.prev` before sending it, so an expired or nearly
   expired previous pair is refused — which is the correct answer: roll back to
   a certificate that is about to lapse is not a rollback.
5. Re-run §7. The serials must now show the previous certificate, on every port.
6. **Make the rollback the current state before the next tick.** The timer still
   reads the main export directory, which holds the *new* pair; leaving it alone
   would re-apply the pair you just rolled back. Either point the main allowlist
   back at the sources (Caddy still serves the new pair, so this only helps after
   Caddy is fixed) or, as the supported path, move the rollback export directory
   over the current one while the timer is stopped:
   `systemctl stop ato-cert-renew.timer`, `mv <instance> <instance>-failed`,
   `mv <instance>-rollback <instance>`, then re-enable the timer. Keep
   `<instance>-failed` (root-only) for the investigation.
7. Delete the rollback allowlist file when done (it points at a specific pair and
   is not meant to persist).

If the rollback pair is no longer usable, re-issue from Caddy by forcing a
renewal, then follow §5 again.

## 10. What this runbook does not claim

- It has never been installed or executed on a host.
- It assumes the API surface and the OCI environment fix from ADR-044 exist.
- It assumes Caddy writes the pair where the allowlist says; the Caddy storage
  layout is not read or copied by the tool.
- The mail ports are not open in staging, so the public-path part of §7 is a
  production/test-domain step (plan, pre-cutover verification).
