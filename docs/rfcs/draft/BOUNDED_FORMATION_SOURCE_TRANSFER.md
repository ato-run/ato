# Bounded Formation source acquisition

Status: draft implementation contract. Local HTTP fixtures are verified;
deployment and live GitHub acceptance are not included in this change.

## Boundary

The Formation worker acquires a pinned public GitHub archive or the upload
already assigned to its job. Acquisition is physical transport; it does not
change the source tree, resolver, closure, Contract, or Derivation identity.
Existing `FileVerifiedArchive` remains the authority for archive verification,
credential exclusion, path/symlink policy, tree measurement and materialization.

GitHub source URLs have the fixed HTTPS host `codeload.github.com`, validated
owner/repository path segments and the Coordinator's resolved lowercase
40-character commit SHA. A moving requested ref never replaces that SHA.
GitHub requests receive no control-plane credential. Upload acquisition uses
the fixed job-owned API path and its existing Formation bearer credential.
All redirects are rejected, including same-host redirects.

The dedicated source client disables inherited proxies, referers and HTTP
decompression. It requests identity encoding and counts the actual response
body bytes before archive decompression. Only HTTP 200 is accepted. An
excessive Content-Length is rejected before body acquisition; chunked and
unknown-length bodies are limited by their actual bytes.

The per-job compressed limit is the smaller of the existing Runtime source
object ceiling (256 MiB) and `SourceLimits.max_total_bytes`. This uses existing
configuration; expanded archive limits remain independently enforced by
`SourceLimits`. No provider/application-specific limit is introduced.
The frozen source deadline is the smaller of `BuildLimits.wall_clock_seconds`
and the existing 15-minute source-transfer ceiling. It covers response headers,
the complete body, archive verification and materialization. A body read does
not reset the deadline; network requests carry its remaining total timeout.

Production acquisition uses a fixed 64 KiB buffer, reads no more than the
compressed limit plus one overflow-detection byte, writes no more than the
limit, and incrementally hashes those bytes. It never sizes an allocation from
Content-Length or accumulates the archive in a Vec. The file is created under
the configured work root's explicit `.tmp` directory, after verifying that the
directory does not resolve outside that root and available space covers the
limit. The anonymous file is removed by handle cleanup on success/error/drop.
Existing unrelated files and work roots are not deleted.

The source-fetcher trait retains its legacy Vec entry point for trusted
in-process fixtures. Its default file adapter checks the returned size and
deadline before writing. This does not constrain an arbitrary fixture's
internal allocations; the production implementation overrides that method
with the streaming transport above. The production compatibility Vec entry
point also enforces the existing byte/time ceiling, but the job path uses the
file entry point.

## Verification scope

Tests exercise an actual loopback HTTP server for oversized Content-Length,
chunked/unknown-length overflow, every redirect status, header/body stalls,
slow-drip total timeout, unchanged encoded bytes and job-owned upload routing.
A synthetic endless reader establishes the exact read/write bound. A gzip
archive fixture establishes unchanged tree/resolver/subdirectory closure
identity and file materialization through the existing authority. Fixtures
also check anonymous-file cleanup, scratch-directory symlink refusal, public
GitHub URL pinning and credential isolation.

These tests do not establish staging deployment, live GitHub availability,
Linux sandbox containment, or application Contract satisfaction. Transfer
failure cannot proceed to build or publication; it follows the existing
Formation source-failure path.
