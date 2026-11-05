#!/bin/sh
set -eu
repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"
image=${BEVEREN_TEST_IMAGE:-beveren-fsm-single-amd64-redis-proof:20261106}
suffix=$(python3 -c 'import secrets;print(secrets.token_hex(6))')
volume="beveren-proof-$suffix"
server="beveren-proof-$suffix-app"
cleanup() {
    result=$?
    trap - EXIT
    if [ "$result" -eq 0 ] && [ "${BEVEREN_KEEP_TEST:-0}" != 1 ]; then
        docker rm -fv "$server" >/dev/null 2>&1 || :
        docker volume rm "$volume" >/dev/null 2>&1 || :
    else
        printf 'Synthetic test resources: volume=%s, app=%s\n' "$volume" "$server" >&2
    fi
    exit "$result"
}
if docker volume inspect "$volume" >/dev/null 2>&1 || docker inspect "$server" >/dev/null 2>&1; then
    printf 'Test resource name collision; refusing to continue.\n' >&2
    exit 1
fi
trap cleanup EXIT
docker volume create "$volume" >/dev/null
docker run --rm --platform linux/amd64 --network none --read-only --user root --mount "type=volume,src=$volume,dst=/data" --entrypoint sh "$image" -c 'chown 1000:1000 /data && chmod 700 /data'
docker run --rm --platform linux/amd64 --network none --read-only --cap-drop ALL --security-opt no-new-privileges --user 1000:1000 --mount "type=volume,src=$volume,dst=/data" --entrypoint sh "$image" -c 'set -eu; mkdir /data/tmp; TMPDIR=/data/tmp mariadb-install-db --no-defaults --datadir=/data/db --basedir=/usr --auth-root-authentication-method=socket --auth-root-socket-user=frappe --skip-test-db > /data/db-init.log 2>&1'
docker run -d --platform linux/amd64 --name "$server" --network none --read-only --cap-drop ALL --security-opt no-new-privileges --user 1000:1000 --mount "type=volume,src=$volume,dst=/data" --entrypoint /usr/sbin/mariadbd "$image" --no-defaults --datadir=/data/db --basedir=/usr --socket=/data/mysql.sock --pid-file=/data/mysql.pid --tmpdir=/data/tmp --skip-networking --skip-name-resolve --log-error=/data/mysql.log --innodb-buffer-pool-size=128M >/dev/null
docker exec "$server" sh -c 'i=0; until mariadb --no-defaults --protocol=socket --socket=/data/mysql.sock -ufrappe --batch --skip-column-names -e "SELECT 1" >/dev/null 2>&1; do i=$((i+1)); [ "$i" -lt 60 ] || exit 1; sleep 1; done'
docker exec "$server" mkdir /data/redis
cat samples/discover-oss/beveren-fsm/local-test/single-volume/redis_probe.py | docker exec -i "$server" sh -c 'cat > /data/tmp/redis_probe.py'
docker exec -d -e LD_LIBRARY_PATH=/opt/redis-libs "$server" /opt/redis/redis-server --bind 127.0.0.1 --port 6379 --dir /data/redis --appendonly yes --appendfsync always --save '' --protected-mode yes
docker exec -e TMPDIR=/data/tmp "$server" /home/frappe/frappe-bench/env/bin/python /data/tmp/redis_probe.py set
docker exec -i "$server" sh -s <<'SETUP'
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
redis_url = 'redis://127.0.0.1:6379'
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
mariadb --no-defaults --protocol=socket --socket=/data/mysql.sock -ufrappe -e "DROP USER 'ato_setup'@'localhost'; DROP USER 'frappe'@'localhost'"
rm /data/test-db-bootstrap /data/tmp/create_bootstrap.py
if mariadb --no-defaults --protocol=socket --socket=/data/mysql.sock -ufrappe -e 'SELECT 1' >/dev/null 2>&1; then
    printf 'Unexpected privileged socket access after setup.\n' >&2
    exit 1
fi
if mariadb --no-defaults --protocol=socket --socket=/data/mysql.sock -uroot -e 'SELECT 1' >/dev/null 2>&1; then
    printf 'Unexpected root socket access from the app user.\n' >&2
    exit 1
fi
cat > /data/tmp/check_database.py <<'PYDB'
import json,pymysql
with open('/data/bench/sites/single2.local/site_config.json') as f:
    config=json.load(f)
connection=pymysql.connect(unix_socket='/data/mysql.sock',user=config['db_name'],
                           password=config['db_password'],database=config['db_name'])
with connection.cursor() as cursor:
    cursor.execute('SELECT DATABASE()')
    assert cursor.fetchone()[0] == config['db_name']
    cursor.execute('SHOW GRANTS')
    grants=[row[0] for row in cursor.fetchall()]
    global_grants=[grant for grant in grants if ' ON *.* ' in grant]
    assert global_grants and all('GRANT USAGE ON *.*' in grant for grant in global_grants)
    assert all('WITH GRANT OPTION' not in grant for grant in grants)
connection.close()
PYDB
/data/bench/env/bin/python /data/tmp/check_database.py
printf 'Synthetic x86_64 Beveren installed; bootstrap users removed and site database access verified.\n'
SETUP
docker exec -d -w /data/bench -e TMPDIR=/data/tmp -e PYTHONDONTWRITEBYTECODE=1 -e FRAPPE_BENCH_ROOT=/data/bench -e CI=1 "$server" sh -c 'exec bench --site single2.local serve --port 8000 --noreload > /data/bench-web.log 2>&1'
docker exec "$server" sh -c 'i=0; until /data/bench/env/bin/python -c "import requests;requests.get(\"http://127.0.0.1:8000/login\",timeout=3).raise_for_status()" >/dev/null 2>&1; do i=$((i+1)); [ "$i" -lt 60 ] || exit 1; sleep 1; done'
sed 's|http://web:8000|http://127.0.0.1:8000|' samples/discover-oss/beveren-fsm/local-test/setup_synthetic.py | docker exec -i "$server" sh -c 'cat > /data/tmp/setup_synthetic.py'
sed 's|http://web:8000|http://127.0.0.1:8000|' samples/discover-oss/beveren-fsm/local-test/workflow.py | docker exec -i "$server" sh -c 'cat > /data/tmp/workflow.py'
cat samples/discover-oss/beveren-fsm/local-test/single-volume/persist_probe.py | docker exec -i "$server" sh -c 'cat > /data/tmp/persist_probe.py'
docker exec -w /data/bench "$server" sh -c 'set -eu; export TMPDIR=/data/tmp PYTHONDONTWRITEBYTECODE=1 FRAPPE_BENCH_ROOT=/data/bench; export BEVEREN_ADMIN_PASSWORD="$(cat /data/test-admin-password)"; /data/bench/env/bin/python /data/tmp/setup_synthetic.py; /data/bench/env/bin/python /data/tmp/workflow.py; /data/bench/env/bin/python /data/tmp/persist_probe.py create'
docker stop --timeout 30 "$server" >/dev/null
docker start "$server" >/dev/null
docker exec -d -e LD_LIBRARY_PATH=/opt/redis-libs "$server" /opt/redis/redis-server --bind 127.0.0.1 --port 6379 --dir /data/redis --appendonly yes --appendfsync always --save '' --protected-mode yes
docker exec -e TMPDIR=/data/tmp "$server" /home/frappe/frappe-bench/env/bin/python /data/tmp/redis_probe.py verify
docker exec "$server" sh -c 'i=0; until /data/bench/env/bin/python /data/tmp/check_database.py >/dev/null 2>&1; do i=$((i+1)); [ "$i" -lt 60 ] || exit 1; sleep 1; done'
docker exec -d -w /data/bench -e TMPDIR=/data/tmp -e PYTHONDONTWRITEBYTECODE=1 -e FRAPPE_BENCH_ROOT=/data/bench -e CI=1 "$server" sh -c 'exec bench --site single2.local serve --port 8000 --noreload >> /data/bench-web.log 2>&1'
docker exec "$server" sh -c 'i=0; until /data/bench/env/bin/python -c "import requests;requests.get(\"http://127.0.0.1:8000/login\",timeout=3).raise_for_status()" >/dev/null 2>&1; do i=$((i+1)); [ "$i" -lt 60 ] || exit 1; sleep 1; done; export BEVEREN_ADMIN_PASSWORD="$(cat /data/test-admin-password)"; /data/bench/env/bin/python /data/tmp/persist_probe.py verify'
if [ "${BEVEREN_TEST_PATCHED:-0}" = 1 ]; then
    docker exec "$server" mkdir -p /data/bench/sites/.tmp
    for probe in authz_probe.py shared_order_probe.py appointment_probe.py order_mapping_probe.py linked_request_probe.py schedule_probe.py schedule_workflow_probe.py; do
        sed 's|http://web:8000|http://127.0.0.1:8000|' "samples/discover-oss/beveren-fsm/local-test/$probe" |
            docker exec -i "$server" sh -c "cat > /data/bench/sites/.tmp/$probe"
    done
    docker exec -w /data/bench "$server" sh -c 'set -eu; export TMPDIR=/data/tmp PYTHONDONTWRITEBYTECODE=1 FRAPPE_BENCH_ROOT=/data/bench; export BEVEREN_ADMIN_PASSWORD="$(cat /data/test-admin-password)"; for probe in authz_probe.py shared_order_probe.py appointment_probe.py order_mapping_probe.py linked_request_probe.py schedule_probe.py schedule_workflow_probe.py; do /data/bench/env/bin/python "sites/.tmp/$probe"; done'
fi
printf 'Synthetic single-service amd64 proof completed.\n'
