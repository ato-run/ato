# OCI candidate comparison — three existing source pins

**No candidate selected; zero online builds or image pulls.** This is a static
survey of File Browser, Kutt and changedetection.io only. It does not requalify
the other 41 service repositories or revise the old D1/coverage results.

Existing archive SHA256 values were verified. Root Dockerfiles and relevant
scripts/configuration/locks were read without modification. Current code
`b43eaa0c` was checked for admission/authoring boundaries. No source/Dockerfile
rewrite, capability, root override, new host permission or bound increase.

| Candidate / frozen source | External images; compressed layer total amd64 / arm64 | Launch / state from source | Minimum gaps |
|---|---|---|---|
| File Browser `833d908884d5c801f30f5c098d7977177eb3a36b` | alpine:3.23 + busybox:1.37.0-musl: **4,719,597 / 5,085,721 bytes** | ENTRYPOINT `tini -- /init.sh`, no CMD; USER user=1000:1000; port 80; VOLUMEs `/srv`, `/config`, `/database` | `COPY filebrowser` binary absent from archive; ENTRYPOINT-only; three state mounts exceed route's one-slot limit; apk + raw.githubusercontent.com acquisition |
| Kutt `279b491b53bbd01fbae70f603222526962772061` | node:22-alpine: **60,697,390 / 61,053,420 bytes** | shell CMD migration then npm start; `/kutt`; port 3000; no USER/VOLUME in source; default SQLite `db/data` | VOLUME-external state; production JWT_SECRET/runtime bindings; native dependency install scripts, redirects and final artifact/transfer size unestablished |
| changedetection.io `0e0566721b1c483dcf7ae548210ee10532d9b181` | python:3.11-slim-bookworm, reused in two stages: **47,795,628 / 47,450,590 bytes** (unique base counted once) | ENTRYPOINT wrapper + Python CMD; `/app`; port 5000; no USER/VOLUME in source; `/datastore` | VOLUME-external state; apt/pip and additional indexes/redirects; final image/transfer unknown; normal external monitoring needs runtime egress |

None declares `# syntax=...`; the default BuildKit frontend is implicit. Default
final targets and default build args were reviewed. changedetection.io's
`PYTHON_VERSION=3.11`, `LOGGER_LEVEL=''`, and automatic `TARGETPLATFORM` do not
need a caller override from the source text. Its interpolated FROM was resolved
for metadata; actual named-context binding/build was not exercised.
Single base, CMD and VOLUME are selection conveniences, **not universal OCI
requirements**. Current source-to-OCI authoring specifically requires nonempty
image Cmd and exact equality between VOLUMEs and explicitly authorized state;
its request currently permits at most one state slot. The Adapter keeps readonly
root, cap-drop ALL and no-new-privileges. `/tmp` tmpfs is transient, not durable
state.

Image configuration/final file capabilities/effective UID/GID were not
validated: no image was built or config/layer blob fetched. Root Dockerfiles do
not run setcap or request privileged/host-network execution; this is not a
verified absence of inherited capabilities. State-backed OCI runs derive an
effective UID/GID from the existing mount binding; source USER alone is not the
effective-identity proof.

Kutt's 431 lock `resolved` URLs use registry.npmjs.org, but that does not bound
install-script traffic. better-sqlite3 and msgpackr-extract declare install
scripts; their prebuilt/native fallback and external endpoints remain
unverified. Its default SQLite path is `/kutt/db`; `/var/lib/kutt` is created by
the Dockerfile but is not the default DB_FILENAME and is also not a VOLUME.
JWT_SECRET has only a development default; the source uses production mode.
Explicit runtime binding/authoring is needed without capturing secrets as
identity. Redis/Postgres are optional, not required for the default SQLite
shape.

File Browser's root recipe packages an already built binary; it does not compile
Go/Vue from this pinned archive. The old D1 “structural candidate / no unresolved
requirements” classification did not establish this input closure. This is new
evidence of a missed input requirement; that historical survey is left intact.
ENTRYPOINT-only support alone would still leave the binary, state-count and
network gaps.

changedetection.io installs apt packages, pip requirements, optional Playwright
and OpenCV, with explicit extra indexes www.piwheels.org and pypi.anaconda.org.
No new request was made to them, Debian or Python download hosts. Redirects and
transitive toolchains are not a closed allowlist. EXTRA_PACKAGES must remain
unset to avoid runtime installation. Optional browser/LLM integrations were not
used; no model call or model credential read occurred.

## Registry metadata and size limits

[Metadata evidence](formation-oci-candidates-registry-2026-09-30.json) records
full root/platform digests and layer descriptors for both platforms on
2026-09-30. Only HTTPS auth.docker.io and registry-1.docker.io were contacted:
anonymous token plus manifest GETs, redirects refused, 62,759 response-body
bytes, no stored credentials and no config/layer bodies. This metadata scope
does not authorize base acquisition or an online build. Blob redirect hosts
were not queried and must be fixed before any future acquisition.

These compressed base totals are smaller than WBO's **app-specific** 200 MiB
base-acquisition allowance, but that allowance is not transferred to another
app. A compressed base size cannot certify the 512 MiB uncompressed transport
archive or the 5 GiB build disk, nor the dependency-inclusive 500 MiB transfer
condition. No final image/artifact exists for these pins, so final/archive size
is explicitly unknown. No build-failure or functional-success count is assigned.

[Full candidate facts](formation-oci-candidates-2026-09-30.json) retain archive /
Dockerfile digests, CMD/ENTRYPOINT/WorkingDir/USER, dependencies, state, port and
uncertainties. ENTRYPOINT-only and VOLUME-external state remain generic design
candidates; this survey does not establish a complete app that those two changes
alone would make viable. No new implementation PR or online plan is opened.
