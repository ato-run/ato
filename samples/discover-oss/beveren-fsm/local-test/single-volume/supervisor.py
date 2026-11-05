#!/usr/bin/env python3
"""Experimental single-container supervisor for isolated synthetic tests only.

This is not a hosted or production runtime. An interrupted initialization or
unclean stop deliberately fails closed and needs an operator recovery design.
"""
import fcntl
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import time

import pymysql

DATA = Path('/data')
BENCH = DATA / 'bench'
SITE = 'single2.local'
WORKSPACE = Path(__file__).resolve().parent
PYTHON = '/home/frappe/frappe-bench/env/bin/python'
CHILDREN = []
stopping = False


class LocalProofError(Exception):
    """A fixed diagnostic that does not contain user-provided values."""


def fail(message):
    raise LocalProofError(message)


def handle_signal(_signum, _frame):
    global stopping
    stopping = True


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def marker(path):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    try:
        os.write(fd, b'1\n')
        os.fsync(fd)
    finally:
        os.close(fd)
    sync_dir(path.parent)


def start(name, args, env, cwd=None):
    logfile = (DATA / 'private' / (name + '.log')).open('ab', buffering=0)
    process = subprocess.Popen(args, env=env, cwd=cwd, stdin=subprocess.DEVNULL,
                               stdout=logfile, stderr=subprocess.STDOUT, start_new_session=True)
    logfile.close()
    CHILDREN.append((name, process))
    return process


def wait_for(probe, process, seconds=60):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if stopping or process.poll() is not None:
            fail('A required local process stopped during startup')
        try:
            if probe():
                return
        except (OSError, pymysql.MySQLError, ValueError):
            pass
        time.sleep(0.25)
    fail('A required local process did not become ready')


def redis_ready():
    with socket.create_connection(('127.0.0.1', 6379), 1) as connection:
        connection.sendall(b'*1\r\n$4\r\nPING\r\n')
        return connection.recv(16) == b'+PONG\r\n'


def db_connect(initial):
    if initial:
        return pymysql.connect(unix_socket='/data/mysql.sock', user='frappe', connect_timeout=1)
    with (BENCH / 'sites' / SITE / 'site_config.json').open() as source:
        config = json.load(source)
    return pymysql.connect(unix_socket='/data/mysql.sock', user=config['db_name'],
                           password=config['db_password'], database=config['db_name'], connect_timeout=1)


def db_ready(initial):
    connection = db_connect(initial)
    try:
        with connection.cursor() as cursor:
            cursor.execute('SELECT 1')
            return cursor.fetchone()[0] == 1
    finally:
        connection.close()


def prepare_layout():
    for relative in ('private', 'tmp', 'redis', 'bench/sites', 'bench/config/pids', 'bench/logs'):
        (DATA / relative).mkdir(mode=0o700, parents=True, exist_ok=True)
    for name in ('apps', 'env', 'assets'):
        (BENCH / name).symlink_to('/home/frappe/frappe-bench/' + name)
    (BENCH / 'sites/assets').symlink_to('/home/frappe/frappe-bench/assets')
    (BENCH / 'sites/apps.txt').write_text('frappe\nerpnext\nbeveren_fsm\n')
    config = {'db_host': 'localhost', 'db_socket': '/data/mysql.sock',
              'redis_cache': 'redis://127.0.0.1:6379', 'redis_queue': 'redis://127.0.0.1:6379',
              'redis_socketio': 'redis://127.0.0.1:6379'}
    (BENCH / 'sites/common_site_config.json').write_text(json.dumps(config))


def run_setup(env, password):
    child_env = dict(env, ATO_BINDING_INITIAL_ADMIN_PASSWORD=password)
    process = start('bootstrap', [PYTHON, str(WORKSPACE / 'bootstrap_binding.py'), SITE],
                    child_env, cwd=str(BENCH / 'sites'))
    while process.poll() is None:
        if stopping:
            fail('Local initialization was interrupted')
        time.sleep(0.5)
    if process.returncode:
        fail('Local initialization failed')
    connection = db_connect(True)
    try:
        with connection.cursor() as cursor:
            cursor.execute("DROP USER 'frappe'@'localhost'")
        connection.commit()
    finally:
        connection.close()
    for account in ('frappe', 'root'):
        try:
            connection = pymysql.connect(unix_socket='/data/mysql.sock', user=account, connect_timeout=1)
        except pymysql.MySQLError:
            continue
        connection.close()
        fail('Unexpected database administrator access')
    connection = db_connect(False)
    try:
        with connection.cursor() as cursor:
            cursor.execute('SHOW GRANTS')
            grants = [row[0] for row in cursor.fetchall()]
            global_grants = [grant for grant in grants if ' ON *.* ' in grant]
            if not global_grants or any('GRANT USAGE ON *.*' not in grant for grant in global_grants):
                fail('Unexpected global database privileges')
            if any('WITH GRANT OPTION' in grant for grant in grants):
                fail('Unexpected database grant privilege')
    finally:
        connection.close()


def stop_children():
    forced = False
    # Web/scheduler/workers first, then database and Redis.
    for name, process in reversed(CHILDREN):
        if process.poll() is not None:
            if name != 'bootstrap':
                forced = True
            continue
        try:
            os.killpg(process.pid, signal.SIGTERM)
            process.wait(timeout=20)
            if process.returncode not in (0, -signal.SIGTERM):
                forced = True
        except ProcessLookupError:
            pass
        except subprocess.TimeoutExpired:
            forced = True
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
    return not forced


def main():
    os.umask(0o077)
    signal.signal(signal.SIGTERM, handle_signal)
    signal.signal(signal.SIGINT, handle_signal)
    password = os.environ.pop('ATO_BINDING_INITIAL_ADMIN_PASSWORD', None)
    if not DATA.is_dir() or DATA.is_symlink():
        fail('Local state volume is missing')
    lock = os.open(DATA / '.local-proof.lock', os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            fail('The local state volume is already in use')
        if (DATA / 'private').is_symlink() or (DATA / 'private/initialized').is_symlink():
            fail('Unexpected local private state')
        first = not (DATA / 'private/initialized').is_file()
        if first:
            if not password or not 16 <= len(password) <= 256 or '\x00' in password:
                fail('An initial administrator binding of 16–256 characters is required')
            if any(item.name != '.local-proof.lock' for item in DATA.iterdir()):
                fail('The initial local state volume is not empty')
            prepare_layout()
            marker(DATA / 'private/initializing')
            result = subprocess.run(['/usr/bin/mariadb-install-db', '--no-defaults', '--datadir=/data/db',
                '--basedir=/usr', '--auth-root-authentication-method=socket',
                '--auth-root-socket-user=frappe', '--skip-test-db'],
                env=dict(os.environ, TMPDIR='/data/tmp'), stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            if result.returncode or stopping:
                fail('Local database initialization failed')
        else:
            for relative in ('private', 'tmp', 'redis', 'bench', 'bench/sites', 'bench/config', 'bench/logs'):
                item = DATA / relative
                if item.is_symlink() or not item.is_dir() or not item.resolve().is_relative_to(DATA.resolve()):
                    fail('Unexpected local state layout')
            if (DATA / 'private/initializing').exists() or (DATA / 'private/running').exists():
                fail('Unclean local state requires operator recovery')
        marker(DATA / 'private/running')
        env = dict(os.environ, TMPDIR='/data/tmp', PYTHONDONTWRITEBYTECODE='1',
                   FRAPPE_BENCH_ROOT='/data/bench', PYTHONPATH=str(WORKSPACE))
        # Frappe changes background-job behavior when CI is set. Gunicorn does
        # not need the development server's CI switch to disable its debugger.
        env.pop('CI', None)
        redis = start('redis', ['/opt/redis/redis-server', '--bind', '127.0.0.1', '--port', '6379',
            '--dir', '/data/redis', '--appendonly', 'yes', '--appendfsync', 'always',
            '--save', '', '--protected-mode', 'yes'], dict(env, LD_LIBRARY_PATH='/opt/redis-libs'))
        wait_for(redis_ready, redis)
        db = start('database', ['/usr/sbin/mariadbd', '--no-defaults', '--datadir=/data/db',
            '--basedir=/usr', '--socket=/data/mysql.sock', '--pid-file=/data/mysql.pid',
            '--tmpdir=/data/tmp', '--skip-networking', '--skip-name-resolve',
            '--log-error=/data/private/mysql.log', '--innodb-buffer-pool-size=128M'], env)
        wait_for(lambda: db_ready(first), db)
        if first:
            run_setup(env, password)
            marker(DATA / 'private/initialized')
            (DATA / 'private/initializing').unlink()
            sync_dir(DATA / 'private')
        password = None
        for queue in ('short', 'default', 'long'):
            start('worker-' + queue, ['bench', 'worker', '--queue', queue], env, cwd=str(BENCH))
        start('scheduler', ['bench', 'schedule'], env, cwd=str(BENCH))
        start('web', [str(BENCH / 'env/bin/gunicorn'), '--bind', '0.0.0.0:8000', '--workers', '2',
                      '--threads', '2', '--timeout', '60', 'wsgi_single:application'],
              env, cwd=str(BENCH / 'sites'))
        while not stopping:
            if any(process.poll() is not None for name, process in CHILDREN if name != 'bootstrap'):
                fail('A required local process stopped')
            time.sleep(1)
        if stop_children():
            (DATA / 'private/running').unlink()
            sync_dir(DATA / 'private')
    finally:
        stop_children()
        os.close(lock)


if __name__ == '__main__':
    try:
        main()
    except LocalProofError as error:
        print(f'Local synthetic supervisor stopped: {error}', file=sys.stderr)
        raise SystemExit(1) from None
    except Exception as error:
        print(f'Local synthetic supervisor stopped: {type(error).__name__}', file=sys.stderr)
        raise SystemExit(1) from None
