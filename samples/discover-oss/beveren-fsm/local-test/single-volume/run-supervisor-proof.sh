#!/bin/sh
set -eu
repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"
workspace="$repo_root/samples/discover-oss/beveren-fsm/local-test/single-volume"
image=${BEVEREN_TEST_IMAGE:-beveren-fsm-single-amd64-redis-proof:20261106}
suffix=$(python3 -c 'import secrets;print(secrets.token_hex(6))')
volume="beveren-supervisor-$suffix-data"
server="beveren-supervisor-$suffix-app"
mkdir -p .tmp
umask 077
secret_file=$(mktemp .tmp/beveren-supervisor.XXXXXX)
cleanup() {
    result=$?
    trap - EXIT
    if [ "$result" -eq 0 ] && [ "${BEVEREN_KEEP_TEST:-0}" != 1 ]; then
        docker rm -fv "$server" >/dev/null 2>&1 || :
        docker volume rm "$volume" >/dev/null 2>&1 || :
        unlink "$secret_file" || :
    else
        printf 'Synthetic test resources: volume=%s, app=%s, private env file=%s\n' "$volume" "$server" "$secret_file" >&2
    fi
    exit "$result"
}
if docker volume inspect "$volume" >/dev/null 2>&1 || docker inspect "$server" >/dev/null 2>&1; then
    printf 'Test resource name collision; refusing to continue.\n' >&2
    unlink "$secret_file"
    exit 1
fi
trap cleanup EXIT
python3 - "$secret_file" <<'PYSECRET'
import secrets,sys
with open(sys.argv[1],'w') as f:
    f.write('ATO_BINDING_INITIAL_ADMIN_PASSWORD='+secrets.token_urlsafe(32)+'\n')
PYSECRET
docker volume create "$volume" >/dev/null
docker run --rm --platform linux/amd64 --network none --read-only --user root --mount "type=volume,src=$volume,dst=/data" --entrypoint sh "$image" -c 'chown 1000:1000 /data && chmod 700 /data'
docker run -d --platform linux/amd64 --name "$server" --network none --read-only --cap-drop ALL --security-opt no-new-privileges --memory 2g --cpus 2 --pids-limit 256 --user 1000:1000 --mount "type=volume,src=$volume,dst=/data" --mount "type=bind,src=$workspace,dst=/ato/workspace,readonly" --env-file "$secret_file" -e CI=1 --entrypoint /home/frappe/frappe-bench/env/bin/python "$image" /ato/workspace/supervisor.py >/dev/null
wait_ready() {
    docker exec -e TMPDIR=/data/tmp "$server" sh -c 'i=0; until /home/frappe/frappe-bench/env/bin/python -c "import requests;requests.get(\"http://127.0.0.1:8000/login\",timeout=3).raise_for_status()" >/dev/null 2>&1; do i=$((i+1)); [ "$i" -lt 240 ] || exit 1; sleep 1; done'
}
wait_ready
if docker run --rm --platform linux/amd64 --network none --read-only --cap-drop ALL --security-opt no-new-privileges --user 1000:1000 --mount "type=volume,src=$volume,dst=/data" --mount "type=bind,src=$workspace,dst=/ato/workspace,readonly" --env-file "$secret_file" --entrypoint /home/frappe/frappe-bench/env/bin/python "$image" /ato/workspace/supervisor.py >/dev/null 2>&1; then
    printf 'Concurrent volume access unexpectedly succeeded.\n' >&2
    exit 1
fi
for probe in setup_synthetic.py workflow.py authz_probe.py order_mapping_probe.py linked_request_probe.py; do
    sed 's|http://web:8000|http://127.0.0.1:8000|' "samples/discover-oss/beveren-fsm/local-test/$probe" | docker exec -i "$server" sh -c "mkdir -p /data/bench/sites/.tmp; cat > /data/bench/sites/.tmp/$probe"
done
set -a
. "$secret_file"
set +a
export BEVEREN_ADMIN_PASSWORD="$ATO_BINDING_INITIAL_ADMIN_PASSWORD"
unset ATO_BINDING_INITIAL_ADMIN_PASSWORD
docker exec -w /data/bench -e BEVEREN_ADMIN_PASSWORD -e TMPDIR=/data/tmp -e PYTHONDONTWRITEBYTECODE=1 -e FRAPPE_BENCH_ROOT=/data/bench "$server" sh -c 'set -eu; /data/bench/env/bin/python sites/.tmp/setup_synthetic.py; /data/bench/env/bin/python sites/.tmp/workflow.py; /data/bench/env/bin/python /ato/workspace/persist_probe.py create; /data/bench/env/bin/python sites/.tmp/linked_request_probe.py'
docker exec -w /data/bench -e TMPDIR=/data/tmp -e PYTHONDONTWRITEBYTECODE=1 -e FRAPPE_BENCH_ROOT=/data/bench -e PYTHONPATH=/ato/workspace "$server" /data/bench/env/bin/python /ato/workspace/enqueue_probe.py
docker exec -e TMPDIR=/data/tmp "$server" /data/bench/env/bin/python /ato/workspace/redis_probe.py set
docker exec "$server" sh -c 'i=0; until test -f /data/bench/sites/single2.local/private/files/synthetic-queue-marker.txt; do i=$((i+1)); [ "$i" -lt 30 ] || exit 1; sleep 1; done'
docker stop --timeout 60 "$server" >/dev/null
test "$(docker inspect --format '{{.State.ExitCode}}' "$server")" -eq 0
docker run --rm --platform linux/amd64 --network none --read-only --user 1000:1000 --mount "type=volume,src=$volume,dst=/data" --entrypoint sh "$image" -c 'test -f /data/private/initialized && test ! -e /data/private/running && test ! -e /data/private/initializing'
docker rm -v "$server" >/dev/null
docker run -d --platform linux/amd64 --name "$server" --network none --read-only --cap-drop ALL --security-opt no-new-privileges --memory 2g --cpus 2 --pids-limit 256 --user 1000:1000 --mount "type=volume,src=$volume,dst=/data" --mount "type=bind,src=$workspace,dst=/ato/workspace,readonly" -e CI=1 --entrypoint /home/frappe/frappe-bench/env/bin/python "$image" /ato/workspace/supervisor.py >/dev/null
wait_ready
docker exec -e BEVEREN_ADMIN_PASSWORD -e TMPDIR=/data/tmp "$server" /data/bench/env/bin/python /ato/workspace/persist_probe.py verify
docker exec -e TMPDIR=/data/tmp "$server" /data/bench/env/bin/python /ato/workspace/redis_probe.py verify
docker exec "$server" sh -c 'test "$(cat /data/bench/sites/single2.local/private/files/synthetic-queue-marker.txt)" = "synthetic queue marker"'
unset BEVEREN_ADMIN_PASSWORD
docker kill "$server" >/dev/null
docker start "$server" >/dev/null
exit_status=$(docker wait "$server")
test "$exit_status" -ne 0
docker run --rm --platform linux/amd64 --network none --read-only --user 1000:1000 --mount "type=volume,src=$volume,dst=/data" --entrypoint sh "$image" -c 'test -f /data/private/initialized && test -f /data/private/running'
printf 'Synthetic supervisor proof completed, including crash refusal.\n'
