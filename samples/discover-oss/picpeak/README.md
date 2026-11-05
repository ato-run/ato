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

On 2026-11-05, the staging import was repeated with an explicitly selected
owner-connected OCI Runner, rather than the managed pool. Instance
`cinst_01M6NA516FCKMJG9ZVVAMQGQPR` reached the ready screen, which displayed
its Contract as verified and the OCI surface as ready. Opening the owner URL
showed PicPeak's first-run setup page. The one-time setup token is required;
no owner-only token retrieval has yet been demonstrated. Do not publish this
Instance as ready for general use or claim that authentication, galleries,
uploads, persistence, restoration or other workflows have been verified.

Attempting an owner export with saved data failed with
`Checkpoint: failed (The Runner that stores this data is not available right now.)`.
The current staging API source dispatches volume maintenance only through the
operator-managed pool, while this Instance runs on a personal Runner. That is
a concrete incompatibility, but the error alone does not establish whether
other Runner capabilities or availability would also prevent a checkpoint.
