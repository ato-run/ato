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
The inventory identifies the local input bytes. The site includes its locally
prepared `source.tar.gz`, LICENSE and third-party notices. The source and build
comparison below does not establish complete third-party license compliance.

The source archive's 828 regular files and executable modes match the complete
upstream Git tree `6437f8b86d1a35be07fbc9354bcfbc17272a3668`, verified against
the non-truncated GitHub tree and independently reconstructed tree digest.
The tree belongs to upstream commit `3c135266c87747ad86a29d72a038cbe5abff9205`;
other upstream commits share the same tree, so the archive alone does not prove
which commit produced the earlier local build. `verify_source.py` performs the
comparison without extracting the archive and checks the archive SHA-256.

The candidate's 103 generated files were also reproduced byte-for-byte from a
clean checkout of that commit and its pinned `package-lock.json`, using the
Node 22.23.2 / npm 10.9.8 image
`node@sha256:43aeff40f4afc22e83f7589a2f37e111cff5ca84529571f1c8415bcc5fcc21b2`.
`npm ci --ignore-scripts` fetched dependencies; the subsequent `npm run build`
ran with no network, a read-only container root and a writable checkout only.
The image has no Git, so `git_version_guard.sh` answered only the build's
`git rev-parse --short HEAD` query after checking the exact detached HEAD.
`verify_rebuild.py` checks the commit, tree, clean tracked files, lock digest,
exact generated-file set and bytes, plus the upstream LICENSE. The source
archive and LICENSE are the only two site files added outside the Vite build.
This is evidence for this pinned build, not a security audit of its dependencies.

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

To independently repeat the source and build comparison, the commands below
use new paths and require network access to GitHub and the npm registry. The
build itself runs without network; the temporary container scratch directory is
an isolated in-memory mount, not the host filesystem.

```sh
git clone --no-checkout --filter=blob:none https://github.com/Lonshaus/aoiko.git .tmp/aoiko-upstream-rebuild
git -C .tmp/aoiko-upstream-rebuild checkout --detach 3c135266c87747ad86a29d72a038cbe5abff9205
gh api 'repos/Lonshaus/aoiko/git/trees/6437f8b86d1a35be07fbc9354bcfbc17272a3668?recursive=1' > .tmp/aoiko-upstream-tree.json
python3 samples/discover-oss/aoiko/verify_source.py .tmp/aoiko-candidate/site/source.tar.gz .tmp/aoiko-upstream-tree.json
docker run --rm --platform linux/amd64 --read-only --cap-drop ALL --security-opt no-new-privileges \
  --user "$(id -u):$(id -g)" --memory 3g --cpus 2 --pids-limit 256 --tmpfs /scratch:rw,nosuid,size=768m \
  --mount type=bind,src="$PWD/.tmp/aoiko-upstream-rebuild",dst=/src --workdir /src \
  -e TMPDIR=/scratch -e npm_config_cache=/scratch/npm-cache \
  node@sha256:43aeff40f4afc22e83f7589a2f37e111cff5ca84529571f1c8415bcc5fcc21b2 npm ci --ignore-scripts --no-audit --no-fund
docker run --rm --platform linux/amd64 --network none --read-only --cap-drop ALL --security-opt no-new-privileges \
  --user "$(id -u):$(id -g)" --memory 3g --cpus 2 --pids-limit 256 --tmpfs /scratch:rw,nosuid,size=768m \
  --mount type=bind,src="$PWD/.tmp/aoiko-upstream-rebuild",dst=/src \
  --mount type=bind,src="$PWD/samples/discover-oss/aoiko/git_version_guard.sh",dst=/usr/local/bin/git,readonly \
  --workdir /src -e TMPDIR=/scratch -e npm_config_cache=/scratch/npm-cache \
  node@sha256:43aeff40f4afc22e83f7589a2f37e111cff5ca84529571f1c8415bcc5fcc21b2 npm run build
python3 samples/discover-oss/aoiko/verify_rebuild.py .tmp/aoiko-candidate/site .tmp/aoiko-upstream-rebuild
```
