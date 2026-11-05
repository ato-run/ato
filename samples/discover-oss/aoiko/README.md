# aoiko static distribution candidate

Status: local experiment; **not imported, published or verified on staging**.
The owner's existing process-backed aoiko Instance is private. On 2026-11-06
opening it from the staging UI returned `instance_wake_failed` with
`wake_timeout` and runtime state `sleeping`. This response does not establish
why the Runner did not become ready. A separate unauthenticated HTTPS request
to its app host completed TLS verification and returned the expected 401 auth
recovery response; that is not an authenticated launch or a browser test.

`prepare_static.py` constructs an experimental static authoring directory from
the previously prepared local aoiko site. It checks the exact set, sizes and
SHA-256 digests of all 105 site files against `site-files.json`, rejects
symlinks, additions and changes, and does not overwrite an existing output.
The inventory identifies the local input bytes; it does **not** establish the
upstream commit, a reproducible upstream build, or complete third-party
license compliance. The site includes its locally prepared `source.tar.gz`,
LICENSE and third-party notices, whose completeness still requires review.

On 2026-11-06 the candidate was packed as a new `ato.portable-application/1`
Static Web Capsule with portable tree schema v2. It has its own ContractRef;
it does not replace or reinterpret the Contract of the process-backed Instance.
An independent comparison decoded every tree entry and confirmed all 105 files
match the input bytes. Repacking a separately prepared directory produced the
same bytes. Its identifiers are:

- Bundle SHA-256: `67ef3b9f12fc89b47aff75b3ff5b586bfbc598eb9a24b6f860fd11ebcff4696b`
- ContractRef: `sha256:700e678ba3dfb0bef00d5d54edba8f9b3faea84308b31179a7da367896192bdd`
- DerivationRef: `sha256:fc6cb5e433b8d3a96562c4c2f4470bd7eca8ff983fd9d89e44ccca3e3beb238e`

In a fresh local test environment, `ato run --no-open` returned a Rust-owned
receipt with `target.kind = cli-local` and `fully_satisfied = true`. Its five
observations covered the entry page, source archive, Japanese OCR model, OCR
Wasm engine, and workspace identity. A separately imported local Instance also
started and showed the aoiko initial page in Chrome. Its disclaimer requires
agreement before data entry; no agreement, journal entry, real financial data,
OCR, export, or persistence workflow was tested. The app reports that its data
is stored in browser IndexedDB, which is not evidence of Ato-hosted backup or
cross-device persistence.

The upstream site's `_headers` declares, among other restrictions, a content
security policy, `Cross-Origin-Opener-Policy`, `X-Frame-Options`, referrer
policy and `X-Content-Type-Options`. The local static runtime's entry response
did not carry any of those five headers. The file's presence in the tree does
not apply its rules. The staging static delivery source supplies some framing,
referrer and MIME protections, but the complete hosted header behavior and the
app's compatibility with it have not been verified. A successful Contract
receipt does not test these security headers. Do not publish this candidate as
ready until its actual hosted response, security requirements, functionality,
source provenance, licensing and user-facing data behavior are validated.

The local experiment can be repeated from the repository root when the exact
site bytes remain available under `.tmp/aoiko-candidate/site`:

```sh
PYTHONDONTWRITEBYTECODE=1 TMPDIR="$PWD/.tmp" python3 -m unittest discover -s samples/discover-oss/aoiko -p 'test_*.py' -v
python3 samples/discover-oss/aoiko/prepare_static.py .tmp/aoiko-candidate/site .tmp/aoiko-static-new
target/debug/ato __static-bundle .tmp/aoiko-static-new --title aoiko --tree-schema v2 -o .tmp/aoiko-static-new.capsule
TMPDIR="$PWD/.tmp" scripts/ato-test-shell.sh target/debug/ato run .tmp/aoiko-static-new.capsule --no-open --verification-receipt .tmp/aoiko-static-new-receipt.json
```

`__static-bundle` is an experimental CLI surface. The output directory and
bundle should be new paths; `prepare_static.py` refuses an existing directory.
