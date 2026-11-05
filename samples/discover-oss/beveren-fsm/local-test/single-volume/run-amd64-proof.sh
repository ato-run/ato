#!/bin/sh
set -eu
repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"
image=${BEVEREN_TEST_IMAGE:-beveren-fsm-single-amd64-proof:20261106}
suffix=$(python3 -c 'import secrets;print(secrets.token_hex(6))')
volume="beveren-proof-$suffix"
server="beveren-proof-$suffix-app"
redis="beveren-proof-$suffix-redis"
network="beveren-proof-$suffix-net"
cleanup() {
    result=$?
    if [ "$result" -eq 0 ] && [ "${BEVEREN_KEEP_TEST:-0}" != 1 ]; then
        docker rm -fv "$server" "$redis" >/dev/null 2>&1 || :
        docker network rm "$network" >/dev/null 2>&1 || :
        docker volume rm "$volume" >/dev/null 2>&1 || :
    else
        printf 'Synthetic test resources: volume=%s, network=%s, app=%s, redis=%s\n' "$volume" "$network" "$server" "$redis" >&2
    fi
}
if docker volume inspect "$volume" >/dev/null 2>&1 || docker network inspect "$network" >/dev/null 2>&1 || docker inspect "$server" >/dev/null 2>&1 || docker inspect "$redis" >/dev/null 2>&1; then
    printf 'Test resource name collision; refusing to continue.\n' >&2
    exit 1
fi
trap cleanup EXIT
redis_image='redis@sha256:987c376c727652f99625c7d205a1cba3cb2c53b92b0b62aade2bd48ee1593232'
docker network create --internal "$network" >/dev/null
docker volume create "$volume" >/dev/null
docker run --rm --platform linux/amd64 --network none --read-only --user root --mount "type=volume,src=$volume,dst=/data" --entrypoint sh "$image" -c 'chown 1000:1000 /data && chmod 700 /data'
docker run --rm --platform linux/amd64 --network none --read-only --cap-drop ALL --security-opt no-new-privileges --user 1000:1000 --mount "type=volume,src=$volume,dst=/data" --entrypoint sh "$image" -c 'set -eu; mkdir /data/tmp; TMPDIR=/data/tmp mariadb-install-db --no-defaults --datadir=/data/db --basedir=/usr --auth-root-authentication-method=socket --auth-root-socket-user=frappe --skip-test-db > /data/db-init.log 2>&1'
docker run -d --name "$redis" --network "$network" --read-only --cap-drop ALL --security-opt no-new-privileges --memory 256m --pids-limit 64 --tmpfs /data:rw,nosuid,size=64m --entrypoint redis-server "$redis_image" --save '' --appendonly no >/dev/null
docker run -d --platform linux/amd64 --name "$server" --network "$network" --read-only --cap-drop ALL --security-opt no-new-privileges --user 1000:1000 --mount "type=volume,src=$volume,dst=/data" --entrypoint /usr/sbin/mariadbd "$image" --no-defaults --datadir=/data/db --basedir=/usr --socket=/data/mysql.sock --pid-file=/data/mysql.pid --tmpdir=/data/tmp --skip-networking --skip-name-resolve --log-error=/data/mysql.log --innodb-buffer-pool-size=128M >/dev/null
docker exec "$server" sh -c 'i=0; until mariadb --no-defaults --protocol=socket --socket=/data/mysql.sock -ufrappe --batch --skip-column-names -e "SELECT 1" >/dev/null 2>&1; do i=$((i+1)); [ "$i" -lt 60 ] || exit 1; sleep 1; done'
docker exec -i -e TEST_REDIS_NAME="$redis" "$server" sh -s <<'SETUP'
set -eu
umask 077
mkdir -p /data/bench/sites /data/bench/logs /data/bench/config/pids
ln -s /home/frappe/frappe-bench/apps /data/bench/apps
ln -s /home/frappe/frappe-bench/env /data/bench/env
ln -s /home/frappe/frappe-bench/assets /data/bench/assets
ln -s /home/frappe/frappe-bench/assets /data/bench/sites/assets
printf 'frappe\nerpnext\nbeveren_fsm\n' > /data/bench/sites/apps.txt
python3 - <<'PYCONFIG'
import json, os
redis_url = 'redis://' + os.environ['TEST_REDIS_NAME'] + ':6379'
config = {'db_host': 'localhost', 'db_socket': '/data/mysql.sock',
          'redis_cache': redis_url, 'redis_queue': redis_url, 'redis_socketio': redis_url}
with open('/data/bench/sites/common_site_config.json', 'w') as f:
    json.dump(config, f)
PYCONFIG
cat > /data/tmp/create_bootstrap.py <<'PY'
import secrets,pymysql
password=secrets.token_hex(32)
with open('/data/test-db-bootstrap','w') as f: f.write(password)
with open('/data/test-admin-password','w') as f: f.write(secrets.token_urlsafe(32))
c=pymysql.connect(unix_socket='/data/mysql.sock',user='frappe',autocommit=True)
with c.cursor() as cursor:
    cursor.execute("CREATE USER 'ato_setup'@'localhost' IDENTIFIED BY %s",(password,))
    cursor.execute("GRANT ALL PRIVILEGES ON *.* TO 'ato_setup'@'localhost' WITH GRANT OPTION")
c.close()
PY
/data/bench/env/bin/python /data/tmp/create_bootstrap.py
cd /data/bench
export TMPDIR=/data/tmp PYTHONDONTWRITEBYTECODE=1 FRAPPE_BENCH_ROOT=/data/bench
admin_password=$(cat /data/test-admin-password)
db_password=$(cat /data/test-db-bootstrap)
bench new-site --db-root-username ato_setup --db-root-password "$db_password" --db-socket /data/mysql.sock --admin-password "$admin_password" --install-app erpnext --set-default single2.local > /data/bench-install.log 2>&1
bench --site single2.local install-app beveren_fsm >> /data/bench-install.log 2>&1
bench --site single2.local list-apps
mariadb --no-defaults --protocol=socket --socket=/data/mysql.sock -ufrappe -e "DROP USER 'ato_setup'@'localhost'"
rm /data/test-db-bootstrap /data/tmp/create_bootstrap.py
printf 'Synthetic x86_64 Beveren installed and bootstrap user removed.\n'
SETUP
docker exec -d -w /data/bench -e TMPDIR=/data/tmp -e PYTHONDONTWRITEBYTECODE=1 -e FRAPPE_BENCH_ROOT=/data/bench -e CI=1 "$server" sh -c 'exec bench --site single2.local serve --port 8000 --noreload > /data/bench-web.log 2>&1'
docker exec "$server" sh -c 'i=0; until /data/bench/env/bin/python -c "import requests;requests.get(\"http://127.0.0.1:8000/login\",timeout=3).raise_for_status()" >/dev/null 2>&1; do i=$((i+1)); [ "$i" -lt 60 ] || exit 1; sleep 1; done'
sed 's|http://web:8000|http://127.0.0.1:8000|' samples/discover-oss/beveren-fsm/local-test/setup_synthetic.py | docker exec -i "$server" sh -c 'cat > /data/tmp/setup_synthetic.py'
sed 's|http://web:8000|http://127.0.0.1:8000|' samples/discover-oss/beveren-fsm/local-test/workflow.py | docker exec -i "$server" sh -c 'cat > /data/tmp/workflow.py'
cat samples/discover-oss/beveren-fsm/local-test/single-volume/persist_probe.py | docker exec -i "$server" sh -c 'cat > /data/tmp/persist_probe.py'
docker exec -w /data/bench "$server" sh -c 'set -eu; export TMPDIR=/data/tmp PYTHONDONTWRITEBYTECODE=1 FRAPPE_BENCH_ROOT=/data/bench; export BEVEREN_ADMIN_PASSWORD="$(cat /data/test-admin-password)"; /data/bench/env/bin/python /data/tmp/setup_synthetic.py; /data/bench/env/bin/python /data/tmp/workflow.py; /data/bench/env/bin/python /data/tmp/persist_probe.py create'
docker stop --timeout 30 "$server" >/dev/null
docker start "$server" >/dev/null
docker exec "$server" sh -c 'i=0; until mariadb --no-defaults --protocol=socket --socket=/data/mysql.sock -ufrappe --batch --skip-column-names -e "SELECT 1" >/dev/null 2>&1; do i=$((i+1)); [ "$i" -lt 60 ] || exit 1; sleep 1; done'
docker exec -d -w /data/bench -e TMPDIR=/data/tmp -e PYTHONDONTWRITEBYTECODE=1 -e FRAPPE_BENCH_ROOT=/data/bench -e CI=1 "$server" sh -c 'exec bench --site single2.local serve --port 8000 --noreload >> /data/bench-web.log 2>&1'
docker exec "$server" sh -c 'i=0; until /data/bench/env/bin/python -c "import requests;requests.get(\"http://127.0.0.1:8000/login\",timeout=3).raise_for_status()" >/dev/null 2>&1; do i=$((i+1)); [ "$i" -lt 60 ] || exit 1; sleep 1; done; export BEVEREN_ADMIN_PASSWORD="$(cat /data/test-admin-password)"; /data/bench/env/bin/python /data/tmp/persist_probe.py verify'
if [ "${BEVEREN_TEST_PATCHED:-0}" = 1 ]; then
    docker exec "$server" mkdir -p /data/bench/sites/.tmp
    for probe in authz_probe.py shared_order_probe.py appointment_probe.py order_mapping_probe.py schedule_probe.py schedule_workflow_probe.py; do
        sed 's|http://web:8000|http://127.0.0.1:8000|' "samples/discover-oss/beveren-fsm/local-test/$probe" |
            docker exec -i "$server" sh -c "cat > /data/bench/sites/.tmp/$probe"
    done
    docker exec -w /data/bench "$server" sh -c 'set -eu; export TMPDIR=/data/tmp PYTHONDONTWRITEBYTECODE=1 FRAPPE_BENCH_ROOT=/data/bench; export BEVEREN_ADMIN_PASSWORD="$(cat /data/test-admin-password)"; for probe in authz_probe.py shared_order_probe.py appointment_probe.py order_mapping_probe.py schedule_probe.py schedule_workflow_probe.py; do /data/bench/env/bin/python "sites/.tmp/$probe"; done'
fi
