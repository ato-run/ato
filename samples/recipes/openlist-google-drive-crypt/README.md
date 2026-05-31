# OpenList Google Drive Crypt

Self-hosted OpenList uploader backed by encrypted Google Drive storage through the OpenList Crypt driver.

This capsule encrypts files at rest before storing them in Google Drive when files are uploaded through the OpenList Crypt mount. The self-hosted OpenList server can decrypt and preview files, so this is not a zero-knowledge E2E system against the server operator.

Google Drive上には暗号化済みファイルだけを置けますが、OpenListサーバーは復号鍵を持つため、サーバー管理者からも秘匿する用途には使わないでください。

## Launch

Set the admin password as an Ato secret or provide it when prompted:

```bash
ato run openlist-google-drive-crypt
```

For local development from this recipe directory:

```bash
OPENLIST_ADMIN_PASSWORD='replace-with-a-strong-password' ato run .
```

Open `http://127.0.0.1:5244/` and sign in with:

- Username: `admin`
- Password: the value of `OPENLIST_ADMIN_PASSWORD`

The default Ato target only exposes OpenList over loopback HTTP. It does not bind ports 80 or 443.

## Ato Runtime Smoke (Required Before Merge)

Do not treat `--plan-only` as sufficient validation for this recipe. Run a real capsule launch, verify HTTP 200, then verify state persistence across restart.

```bash
cd samples/recipes/openlist-google-drive-crypt

export ATO_HOME="$PWD/.tmp/ato-home-openlist"
export OPENLIST_STATE_DIR="$PWD/.tmp/openlist-state"
export OPENLIST_ADMIN_PASSWORD='dummy-password'

mkdir -p "$ATO_HOME" "$OPENLIST_STATE_DIR"

cargo run -p ato-cli -- run ./capsule.toml --yes --state data="$OPENLIST_STATE_DIR"
curl -I -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:5244/

# stop + restart with same ATO_HOME/state binding
cargo run -p ato-cli -- stop --all --force
cargo run -p ato-cli -- run ./capsule.toml --yes --state data="$OPENLIST_STATE_DIR"
curl -I -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:5244/
```

Expected result:

- Both curl checks return `200`.
- OpenList initializes successfully on first run (no DB/config write failure).
- Admin/session/storage settings remain after stop and restart when reusing the same `--state data=...` directory.

OpenList v4.1.0+ runs as UID/GID `1001` by default. If startup fails with a permission error under `/opt/openlist/data`, treat this as a merge blocker for the Ato-capsule path. In that case, use the Compose profile for now and document in PR validation that Ato runtime verification is blocked pending UID/GID-compatible persistent state handling.

## Manual Google Drive + Crypt Setup

Google Drive and Crypt are intentionally configured after launch because OAuth consent and storage policy choices are user-specific.

1. Sign in to OpenList as `admin` with `OPENLIST_ADMIN_PASSWORD`.
2. Open `Manage -> Storage` and add a Google Drive driver.
3. Enable the Google Drive API, then create an OAuth client and obtain the refresh token required by the OpenList Google Drive driver.
4. In Google Drive, create an empty folder named `encrypted_storage`.
5. Add an OpenList Crypt driver and point its remote path at the Google Drive driver's `encrypted_storage` folder.
6. Publish only the Crypt mount, for example `/secure`, to regular users.
7. Upload files that must be encrypted only through the Crypt mount.

Only files uploaded through the Crypt driver are encrypted before they land in Google Drive. If regular users can write directly to the Google Drive driver mount, that path can store plaintext. Do not expose the raw Google Drive mount to regular users.

After storing data, do not change the Crypt password, salt, or encryption configuration. Changing those settings can make existing encrypted data unreadable.

Do not commit or paste Google OAuth tokens, the Crypt password, the Crypt salt, or the OpenList admin password into the repository, receipts, or test logs.

## Public HTTPS Profile

The capsule manifest is intentionally local-first. For a self-hosted public server, use the included Compose profile:

```bash
export OPENLIST_ADMIN_PASSWORD='replace-with-a-strong-password'
export OPENLIST_PUBLIC_DOMAIN='files.example.com'
docker compose --profile public up -d
```

The `caddy` service terminates HTTPS for `https://${OPENLIST_PUBLIC_DOMAIN}` and proxies to `openlist:5244`. It forwards `Host`, `X-Forwarded-Host`, and `X-Forwarded-Proto`; Caddy also preserves normal request headers such as `Range` and `If-Range` unless you explicitly remove them. The included Caddyfile sets `request_body max_size 10GB` for large uploads.

If you use nginx instead, keep these requirements:

```nginx
client_max_body_size 10g;

location / {
    proxy_pass http://openlist:5244;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-Host $host;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_set_header Range $http_range;
    proxy_set_header If-Range $http_if_range;
}
```

For non-standard public ports, pass the full `domain:port` value in `Host` and `X-Forwarded-Host`.

## Verification Checklist

- OpenList starts from the Ato capsule.
- `http://127.0.0.1:5244/` returns HTTP 200 and shows the login UI.
- Missing `OPENLIST_ADMIN_PASSWORD` blocks launch before the container starts.
- `/opt/openlist/data` is backed by persistent Ato state, is writable by OpenList, and survives restart.
- A test Google Drive driver points to an `encrypted_storage` folder.
- A Crypt mount such as `/secure` can upload and preview a PDF or image.
- The corresponding Google Drive files under `encrypted_storage` are not readable as plaintext.
