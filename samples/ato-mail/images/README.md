# Prebuilt images for the D1 sample

Status: build recipes for the images the sample pins. The images are on GHCR
with the tags `2026-11-02` (`2026-11-02-flat` for Stalwart); the pins are their linux/amd64 manifest digests. The
packages are private, so a Runner without registry credentials still needs them
preloaded by digest. This directory makes that preparation reproducible and
records why each image differs from its upstream.

| Sample pin (capsule.toml) | Built from |
|---|---|
| `ghcr.io/koh0920/stalwart-staging@sha256:25723c49…629b` | `Dockerfile.stalwart` ← `docker.io/stalwartlabs/stalwart@sha256:d898e81b…cdfe` (v0.16.22, linux/amd64) |
| `ghcr.io/koh0920/govcraft-missive@sha256:3dc89054…bc7c` | `Dockerfile.missive` ← Govcraft/missive commit `7716106b6c4638426169dac264657437914bbe7f` + the patches here |

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

The first two patches are compatibility fixes for the pinned upstream commit,
not product changes:

- `missive-nondefault-port.patch` — strips the port from the host used in the
  JMAP redirect allowlist. Without it, `jmap_url = "http://stalwart:8080"`
  fails with "Aborting redirect request to unknown host".
- `missive-identity-submission-capability.patch` — advertises the JMAP
  Submission capability on `Identity/get`; Stalwart rejects that method without
  it.

The image defines `ENTRYPOINT ["/usr/local/bin/missive"]`; the OCI Adapter uses
the step's `argv` as the container command, so an image with only CMD would
make `argv = ["serve", ...]` the executable itself.

## Ato identity header (`missive-ato-identity.patch`)

Ato's app gateway removes client-sent identity headers and adds
`X-Ato-Account: <the signed-in person's app account>` to every request it
forwards to an app that opted into identity forwarding (only Ato's gateway can
reach the app). This third patch, applied after the two compatibility patches,
lets Missive accept that header: a person signed in to Ato opens their mailbox
without Missive's login form and without Missive ever holding the person's
mail password.

It adds an optional `[ato_identity]` section to Missive's config. Without the
section Missive behaves exactly as before — password login works and the header
is ignored:

```toml
[ato_identity]
account_header = "x-ato-account"
impersonator = "ato-webmail-broker@example.test"
impersonator_password_env = "MISSIVE_ATO_IMPERSONATOR_PASSWORD"
```

With the section present and the environment variable set, a request carrying
the header is opened through a Stalwart master-user: the JMAP Basic-auth user
name is `<account>%<impersonator>` and the password is the impersonator's. The
header value must be 1–255 printable ASCII characters with exactly one `@` and
no `%` (anything else answers 400), a JMAP connection failure answers 502, and
the login form is never shown on the header path. If the environment variable is
unset or empty, the section is ignored with one startup warning.

The impersonator is a dedicated Stalwart principal that needs the
`authenticate` and `impersonate` permissions for the header path, created for
example with enabled permissions
`{"@type":"Replace","enabledPermissions":{"authenticate":true,"impersonate":true}}`.
It needs more for account provisioning (see below). The broker password is
delivered per Run through the environment variable and is never stored in
Missive's session.

## Ato account provisioning and app passwords (`missive-ato-accounts.patch`)

The fourth patch, applied after the identity patch, implements the mailbox side
of Ato's account lifecycle. Only Ato's Coordinator calls it: the gateway never
forwards a client request whose path starts with `/__ato/` to an OCI app. Every
route answers JSON, and all of them are unregistered (404) unless
`[ato_identity]` is configured.

| Route | Result |
|---|---|
| `PUT /__ato/accounts/{account}` body `{"display_name": string\|null}` | 201 `{"account","created":true}` when created, 200 `{"account","created":false}` when the account already exists (idempotent), 403 `{"error":"domain_not_provisioned"}` unless the address domain is in `provision_domains`, 400 `{"error":"invalid_account"}` when the address does not have exactly one `@` or contains `%` |
| `GET /__ato/accounts/{account}/app-passwords` | 200 `{"app_passwords":[{"id","description","created_at","expires_at"}]}`; never the secret |
| `POST /__ato/accounts/{account}/app-passwords` body `{"description","expires_at"}` | 201 `{"id","description","created_at","expires_at","secret"}`; this is the only response that carries the secret |
| `DELETE /__ato/accounts/{account}/app-passwords/{id}` | 204; an unknown id is also 204 so Ato can retry |
| `DELETE /__ato/accounts/{account}/app-passwords` | 204 after revoking every app password (used when Ato suspends the account) |

`MISSIVE_ATO_PROVISION_DOMAINS` (comma-separated) overrides
`[ato_identity] provision_domains` when it is set, so the Mail app can set it
per service: changing the variable replaces only Missive's definition, whereas
editing the shared workspace config would change the workspace and restart the
whole Mail app, Stalwart included.

The provisioning PUT creates the Stalwart principal with a random 32-byte
password from the OS CSPRNG; it is never logged, stored, or returned. The
app-password routes run impersonated as `{account}` and require the account
header to equal the `{account}` path segment (403
`{"error":"account_mismatch"}` otherwise). Provider failures answer 502
`{"error":"provider_error","detail":"<JMAP error type>"}`.

The operator must give the impersonator principal Stalwart's **User role** plus
the `impersonate`, `sysAccountCreate` and `sysDomainGet` permissions: the User
role makes it a principal that may own accounts, and Stalwart refuses to create
an account with more rights than its creator. `sysDomainGet` is always needed:
the provisioning PUT looks the address domain up with `x:Domain/get` before it
creates the account. Without it Stalwart answers `forbidden` and the route
answers 502 with `"detail":"forbidden"`.

The image digests in `images.lock` (and the pins in `capsule.toml`) are **not
yet rebuilt** for the identity and account patches: they still name the image
built from the two compatibility patches, which neither accepts the header nor
serves `/__ato/`. Rebuild with `build-images.sh`, verify, and update both pins
before the sample relies on them.

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
