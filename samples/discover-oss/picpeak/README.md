# PicPeak staging candidate

This is a candidate for the upstream [PicPeak](https://github.com/PicPeak/picpeak)
all-in-one image (MIT). `capsule.toml` pins the OCI image index digest and
linux/amd64 platform, places the workspace away from the image's `/app`, and
binds a per-instance writable slot at `/data`.

The upstream image stores its SQLite database, media, generated signing secret
and first-run setup token under `/data`; the token is at `/data/db/SETUP_TOKEN`.
A safe owner-only path to retrieve that token has not been verified. Do not
publish this candidate as ready until first-run setup, state isolation,
persistence and restore have been tested on the selected hosted Runner.

On 2026-11-05 a local Docker Desktop test of this image on linux/amd64, under
emulation, reached `/api/health` with a non-root UID, a read-only container
root, `/tmp` tmpfs, and a writable owned `/data` volume. This is not an Ato
Runner receipt. `ato pack` accepted the manifest; a normal local `ato run`
returned `execution_authority_required`. An attempt to import the bundle into
staging returned `portable_runner_unavailable`. No hosted Run or receipt has
been obtained.
