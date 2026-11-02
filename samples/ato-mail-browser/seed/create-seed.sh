#!/usr/bin/env bash
# Create a Stalwart store seed for the browser slice.
#
#   DOMAIN=mail.example.test TEST_PASSWORD=... ADMIN_PASSWORD=... \
#     OUT=/path/to/seed.tar.gz ./create-seed.sh
#
# The seed is the RocksDB store under /var/lib/stalwart with one enabled domain
# and one test account, created through Stalwart's JMAP registry. The staging
# Runner installs the seed as the Instance's initial State Revision before the
# first Run, so the browser slice can sign in without a manual step.
#
# The seed contains credential hashes and the dynamic registry. Keep it out of
# version control and the repository; it is test data, not a product artifact.
#
# The script is for a shell environment with Docker (the build host or an
# operator laptop); it is not used by the Capsule itself.
set -euo pipefail

: "${DOMAIN:?DOMAIN is required (e.g. mail.example.test)}"
: "${TEST_PASSWORD:?TEST_PASSWORD is required}"
: "${ADMIN_PASSWORD:?ADMIN_PASSWORD is required (temporary bootstrap only)}"
: "${OUT:?OUT is required (path of the seed tarball)}"
test_user=${TEST_USER:-demo}
image=${STALWART_IMAGE:-ghcr.io/stalwartlabs/stalwart:v0.16.22}
host_port=${HOST_PORT:-18095}
run_id="seed-$$"
volume="${run_id}-data"
container="${run_id}-server"
config="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/stalwart-config.json"

cleanup() {
  docker rm -f "${container}" >/dev/null 2>&1 || true
  docker volume rm "${volume}" >/dev/null 2>&1 || true
}
trap cleanup EXIT

mkdir -p "$(dirname "${OUT}")"
docker volume create "${volume}" >/dev/null
docker run -d --name "${container}" -p "127.0.0.1:${host_port}:8080" \
  -e "STALWART_RECOVERY_ADMIN=admin:${ADMIN_PASSWORD}" \
  -e "STALWART_PUBLIC_URL=http://127.0.0.1:${host_port}" \
  -v "${config}:/etc/stalwart/config.json:ro" \
  -v "${volume}:/var/lib/stalwart" "${image}" >/dev/null

ready=0
for _ in $(seq 1 60); do
  if curl -fsS -m 2 -o /dev/null "http://127.0.0.1:${host_port}/healthz/live"; then
    ready=1
    break
  fi
  sleep 1
done
if [ "${ready}" != 1 ]; then
  echo "error: Stalwart did not become healthy" >&2
  exit 1
fi

DOMAIN="${DOMAIN}" TEST_USER="${test_user}" TEST_PASSWORD="${TEST_PASSWORD}" \
ADMIN_PASSWORD="${ADMIN_PASSWORD}" HOST_PORT="${host_port}" python3 - <<'PY'
import base64, json, os, sys, urllib.request

base = f"http://127.0.0.1:{os.environ['HOST_PORT']}/jmap/"
auth = base64.b64encode(
    f"admin:{os.environ['ADMIN_PASSWORD']}".encode()
).decode()
domain = os.environ["DOMAIN"]
user = os.environ["TEST_USER"]
password = os.environ["TEST_PASSWORD"]


def call(object_type, operation, arguments):
    body = json.dumps(
        {
            "using": ["urn:ietf:params:jmap:core", "urn:stalwart:jmap"],
            "methodCalls": [[f"x:{object_type}/{operation}", arguments, "c0"]],
        }
    ).encode()
    request = urllib.request.Request(
        base,
        data=body,
        headers={"Authorization": f"Basic {auth}", "Content-Type": "application/json"},
    )
    with urllib.request.urlopen(request, timeout=20) as response:
        return json.load(response)


def get(object_type):
    response = call(object_type, "get", {})
    return response["methodResponses"][0][1]["list"]


domains = [item for item in get("Domain") if item.get("name") == domain]
if domains:
    domain_id = domains[0]["id"]
else:
    result = call("Domain", "set", {"create": {domain: {"name": domain, "isEnabled": True}}})
    domain_id = result["methodResponses"][0][1]["created"][domain]["id"]

accounts = [item for item in get("Account") if item.get("name") == user]
if not accounts:
    call(
        "Account",
        "set",
        {
            "create": {
                user: {
                    "@type": "User",
                    "name": user,
                    "domainId": domain_id,
                    "credentials": {"0": {"@type": "Password", "secret": password}},
                    "roles": {"@type": "User"},
                    "permissions": {"@type": "Inherit"},
                }
            }
        },
    )
print(f"seed account ready: {user}@{domain}")
PY

docker stop "${container}" >/dev/null
docker rm "${container}" >/dev/null
out_dir="$(cd "$(dirname "${OUT}")" && pwd)"
out_name="$(basename "${OUT}")"
docker run --rm -v "${volume}:/data" -v "${out_dir}:/out" nginx:alpine \
  sh -c "tar -czf /out/${out_name} -C /data ."
echo "seed written: ${out_dir}/${out_name}"
echo "The archive contains credential hashes and the registry; do not commit it."
