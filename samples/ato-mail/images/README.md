# Prebuilt images for the D1 sample

Status: build recipes for the images the sample pins. The images are on GHCR
with the tags `2026-11-02` (`2026-11-02-flat` for Stalwart); the pins are their linux/amd64 manifest digests. The
packages are private, so a Runner without registry credentials still needs them
preloaded by digest. This directory makes that preparation reproducible and
records why each image differs from its upstream.

| Sample pin (capsule.toml) | Built from |
|---|---|
| `ghcr.io/koh0920/stalwart-staging@sha256:25723c49…629b` | `Dockerfile.stalwart` ← `docker.io/stalwartlabs/stalwart@sha256:d898e81b…cdfe` (v0.16.22, linux/amd64) |
| `ghcr.io/koh0920/govcraft-missive@sha256:3dc89054…bc7c` | `Dockerfile.missive` ← Govcraft/missive commit `7716106b6c4638426169dac264657437914bbe7f` + the two patches here |

## Why the Stalwart image is a thin layer

- The Runner drops **every** Linux capability and sets `no-new-privileges`.
  Stalwart's upstream binary carries `cap_net_bind_service` as a file
  capability; the kernel refuses to exec it across that boundary. The
  Dockerfile copies the binary **without xattrs**, which keeps the pinned
  executable bytes but removes the capability. The container network namespace
  allows the configured listener ports without it.
- The Runner expands the state seed as its host uid and offers no OCI id-mapped
  state mount, so the image selects `USER 1000:1000`.

The image pushed to GHCR is that build flattened with `docker export` and
`docker import`. The Runner refuses an image-declared `VOLUME` that has no mount
at the same path, and upstream declares `/etc/stalwart` and `/var/lib/stalwart`.

## Why the Missive image is patched

Both patches are compatibility fixes for the pinned upstream commit, not
product changes:

- `missive-nondefault-port.patch` — strips the port from the host used in the
  JMAP redirect allowlist. Without it, `jmap_url = "http://stalwart:8080"`
  fails with "Aborting redirect request to unknown host".
- `missive-identity-submission-capability.patch` — advertises the JMAP
  Submission capability on `Identity/get`; Stalwart rejects that method without
  it.

The image defines `ENTRYPOINT ["/usr/local/bin/missive"]`; the OCI Adapter uses
the step's `argv` as the container command, so an image with only CMD would
make `argv = ["serve", ...]` the executable itself.

## Building

```sh
# Govcraft/missive checkout at 7716106b6c4638426169dac264657437914bbe7f
./build-images.sh /path/to/missive  linux/amd64
```

The script needs BuildKit (`docker buildx`). It copies the patches to
`<checkout>/ato-patches/` as `Dockerfile.missive` expects, builds both images,
and prints each image's platform, user, entrypoint and RepoDigest. Digests are
**not byte-reproducible across builders** (timestamps in the image config), so
record the digests produced on the Runner's platform; the pins in
`images.lock` and `capsule.toml` must match the images actually preloaded.

## Staging preload checklist

1. Build on (or for) `linux/amd64` with `build-images.sh`.
2. Load both images on the dedicated Runner host and tag them as
   `ghcr.io/koh0920/stalwart-staging:<tag>` and
   `ghcr.io/koh0920/govcraft-missive:<tag>`, then verify:
   `docker image inspect <ref> --format '{{.Os}}/{{.Architecture}} {{.Config.User}} {{json .Config.Entrypoint}}'`
   (the digest must equal `images.lock` and `capsule.toml`).
3. Confirm the Stalwart binary has no file capability:
   `getcap /usr/local/bin/stalwart` prints nothing.
4. Only then run the sample; a digest or platform mismatch is an admission
   failure, not a Contract failure.

## Validation (2026-11-02)

`build-images.sh` was run end-to-end from a clean Govcraft/missive checkout at the
pinned commit on an arm64 workstation (the pinned base manifests make the
result `linux/amd64` through emulation):

| Image | platform | user | entrypoint |
|---|---|---|---|
| Stalwart thin | linux/amd64 | 1000:1000 | `["/usr/local/bin/stalwart"]` |
| Missive patched | linux/amd64 | 65532:65532 | `["/usr/local/bin/missive"]` |

`verify-preload.sh` passed on those images, including
`docker run --cap-drop=ALL --entrypoint /usr/local/bin/stalwart <ref> --version`
(the behavioral form of the file-capability requirement). The digests produced
by a local build are **not** the staging pins; only the preloaded staging
images' digests belong in `images.lock` and `capsule.toml`.
