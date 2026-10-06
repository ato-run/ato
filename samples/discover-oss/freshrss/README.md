# FreshRSS (staging-only, `ato.capsule/2`)

FreshRSS 1.30.0 as an OCI derivation for the Runner OCI sandbox (non-root,
read-only root filesystem, all capabilities dropped).

## Staging only

`capsule.toml` pins `localhost:5000/ato-freshrss@sha256:…`, an image that is
**not published to any registry**. It is built from `./image` and has to be
present on the one dedicated direct-egress staging Runner that hosts this
trusted App (`ATO_OCI_DIRECT_EGRESS`). It does not run on any other Runner and
must not be used for public Try, Guest or Discover Runs.

## Build and update the pinned image

1. Build from `image/` (base image is digest-pinned in the `Dockerfile`):
   `docker build -t localhost:5000/ato-freshrss:1.30.0 samples/discover-oss/freshrss/image`
2. Make the image available on the staging Runner's Docker. How the Runner is
   provisioned is operator-owned and is not documented in this repository;
   record the exact procedure here when it is confirmed.
3. Read the resulting digest on the Runner (`docker image inspect
   --format '{{index .RepoDigests 0}}' <image>`) and write it into
   `runtimes."oci.image"` in `capsule.toml`.
4. Bumping FreshRSS means changing the base `FROM … @sha256:` in the
   `Dockerfile`, rebuilding, and repeating steps 2–3. The digest in
   `capsule.toml` is part of the Contract input, so a new digest is a new App
   revision, not an in-place edit.

## Behaviour

- Apache listens on 8080; runtime files and PHP sessions live in `/tmp`.
- Only `/var/www/FreshRSS/data` is written as state (`ato.state.filesystem@1`).
- No cron daemon. `entrypoint.sh` runs FreshRSS's `actualize_script.php` every
  `FRESHRSS_ACTUALIZE_INTERVAL_S` seconds (default 3600, counted from container
  start).
- HTTP observation: `GET /i/` must answer 200 (setup page before install,
  login page after).

## Difference from `samples/recipes/freshrss`

`samples/recipes/freshrss` is the older `schema_version = "0.3"` recipe: it
runs the upstream `freshrss/freshrss:latest` image as root with its own cron.
This sample is `ato.capsule/2` with a derived image for the restricted Runner.
They serve different purposes; removing the old recipe is a separate decision
once its references have been checked.

## Verification status

Not yet verified on a current CLI/Runner: `ato validate`, first-run setup,
feed fetch, stop, resume and data retention. See the pull request for results.
