# ato-cert-export

The local half of the certificate-Binding renewal path. Design: ADR-044
(`docs/rfcs/draft/ADR-044-certificate-binding-automatic-renewal.md`);
operator procedure: `docs/ops/cert-binding-renewal-runbook.md`.

It answers one question for one allowlisted (Instance, domain) pair: is the
certificate/private-key pair on this host safe to hand to a new Run, and what
exactly is it? It performs no network I/O, starts no Run, and never prints key
material except in the one command whose output *is* the Binding update
payload.

Status: local tool, not installed or deployed anywhere.

## What it does

| Step | Behaviour |
|---|---|
| Allowlist | Selects one (Instance, domain) entry from a root-owned file. Nothing outside that entry is readable. |
| Inspect | Runs the host `openssl` to read the leaf serial, dNSName SANs, notBefore/notAfter, subject and issuer, and to compare the certificate's public key with the private key's public key. |
| Validate | Refuses a mismatched key, a SAN that does not cover the domain, an expired or soon-to-expire certificate, a not-yet-valid certificate and an implausible validity window. Every check runs before anything is written. |
| Export | Writes `cert.pem`, `key.pem` (0600) and `meta.json` (0600) into a 0700 directory, and keeps the previous set as `*.prev`. A pair whose source bytes are unchanged is a no-op. |
| Payload | Prints the JSON body an operator-scoped updater sends. It contains both Binding values, so it is stdout-only and the command says so on stderr. |

## Commands

```text
ato-cert-export check                  # validate, write nothing
ato-cert-export export                 # validate, update the export directory
ato-cert-export export --print-binding-update --grant-id <ID> --expected-generation <N>
ato-cert-export print-binding-update   --grant-id <ID> --expected-generation <N>
```

Options: `--allowlist <PATH>` (default `/etc/ato/cert-export/allowlist.conf`),
`--instance <ID>` (required), `--domain <DNS>` (assertion), `--state-root <PATH>`
(default `/var/lib/ato/cert-export`), `--export-dir <PATH>`,
`--openssl <PATH>` (default `/usr/bin/openssl`), `--min-remaining-hours <N>`
(48), `--warn-remaining-days <N>` (14), `--max-validity-days <N>` (400),
`--allow-unprivileged` (tests only).

Exit codes: `0` ok (including `unchanged`), `2` the pair was rejected, `3`
usage or allowlist problem, `4` privilege refusal, `5` host tool or export
directory failure.

## Allowlist format

Root-owned, `0600`, absolute paths. Whitespace separates fields; `#` starts a
comment. The header line is mandatory.

```text
allowlist = "ato.cert-export.allowlist/1"
# instance_id   domain        certificate                   private key                   cert-binding    key-binding
i_01J8ZQ0K      mail.ato.run  /srv/certs/mail.ato.run.crt   /srv/certs/mail.ato.run.key   mail_cert_pem   mail_key_pem
```

Rules: instance ids `[A-Za-z0-9_-]{1,64}`, domains lowercase DNS with at least
two labels and no wildcard, binding ids `[a-z0-9_]{1,64}` and distinct within
an entry, one entry per (instance, domain), and no binding id used twice for one
instance. Duplicates and unknown headers are refused rather than merged.

## Export directory

`<state-root>/<instance>/<domain>/` is not used: the allowlist guarantees one
domain per instance, so the directory is `<state-root>/<instance>/`.

```text
cert.pem         the validated leaf, byte-identical to the source
key.pem          the validated private key, 0600
meta.json        serial, SANs, notBefore/notAfter, subject, issuer, source
                 digests, action, exported_at and the previous record
cert.pem.prev    the previous validated leaf, kept on change
key.pem.prev     0600
meta.json.prev
```

`meta.json` contains no private key material. The key digest it records is a
local change-detection fingerprint, never sent anywhere. `meta.json` is written
last and names the digests of the two PEM files, so `load_export` can refuse a
directory left inconsistent by an interrupted write or an edit.

## Secret handling

- The private key is read by `openssl` from its path. It is never an argument,
  an environment value or a stdin payload, so it does not appear in `ps`.
- Errors carry file paths and openssl's status, never key bytes.
- `check` and `export` print one journal-safe JSON line to stdout and a human
  summary plus warnings to stderr.
- `--print-binding-update` / `print-binding-update` print the payload, private
  key included, to stdout and **move the plan line to stderr**, so a pipe from
  stdout into the updater cannot pick up anything but the payload. The command
  warns on stderr. Redirect it into the updater
  process; never into a file that outlives the request and never into a log.
- The tool refuses to run without root (`--allow-unprivileged` exists only for
  local tests) and, when privileged, refuses an allowlist that is not root-owned
  and root-only, or an `openssl` binary that is not root-owned and not writable
  by others.

## openssl

The host has no offline-buildable X.509 crate, so inspection shells out to
`openssl`. It is checked to be an absolute path, a regular executable file and
— for a privileged run — root-owned with no group/world write bit. The commands
used are `x509 -serial -subject -issuer -startdate -enddate -ext
subjectAltName`, `x509 -pubkey`, and `pkey -noout -passin pass:` /
`pkey -pubout`. The empty passphrase is not a secret; it makes an encrypted key
fail immediately instead of prompting, so a passphrase-protected key is
rejected rather than waited on.

## Tests

```sh
cargo test --offline -p ato-cert-export
cargo clippy --offline -p ato-cert-export --all-targets -- -D warnings
cargo fmt -p ato-cert-export -- --check
```

Unit tests cover the pure parts (allowlist grammar, SAN and wildcard matching,
openssl output parsing, calendar arithmetic, validation thresholds, change
detection, payload shape) with a fake inspector. `tests/openssl_pair.rs`
generates its fixtures per run under `tools/cert-export/.tmp/`: no private key
is committed, and every certificate is valid relative to the moment the test
runs. When no `openssl` is on `PATH` (set `ATO_CERT_EXPORT_OPENSSL` to choose
one), those tests print a skip instead of passing silently. They are
`#[cfg(unix)]`.
