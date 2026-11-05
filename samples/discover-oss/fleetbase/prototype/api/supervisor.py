#!/usr/bin/env python3
"""Experimental local-only supervisor; not a deployable staging artifact."""
import base64
import os
from pathlib import Path
import re
import secrets
import shutil
import signal
import socket
import subprocess
import sys
import time

os.umask(0o077)
root = Path('/data')
private = root / 'private'
state = root / 'fleetbase'
mysql_args = ['/usr/sbin/mysqld', '--no-defaults', '--basedir=/usr',
              '--datadir=/data/db', '--plugin-dir=/usr/lib64/mysql/plugin',
              '--lc-messages-dir=/usr/share/mysql-8.0', '--socket=/data/mysql.sock',
              '--pid-file=/data/mysql.pid', '--skip-networking', '--secure-file-priv=NULL',
              '--log-error=/data/private/mysql.log']
children = []
stopping = False
clean_shutdown = False

def fail(message):
    print(message, file=sys.stderr, flush=True)
    raise RuntimeError(message)

def on_signal(_sig, _frame):
    global stopping
    stopping = True

signal.signal(signal.SIGTERM, on_signal)
signal.signal(signal.SIGINT, on_signal)

def sync_private_directory():
    fd = os.open(private, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)

def mark_active():
    path = private / 'unclean'
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        os.write(fd, b'active run')
        os.fsync(fd)
    finally:
        os.close(fd)
    sync_private_directory()

def secret(name, value=None):
    path = private / name
    if value is not None:
        path.write_text(value)
        path.chmod(0o600)
    return path.read_text()

def sanitized_log(data, values):
    text = data.decode('utf-8', errors='replace')
    for value in values:
        if value:
            text = text.replace(value, '[REDACTED]')
    with (private / 'commands.log').open('a') as log:
        log.write(text)

def mysql(password, sql, expired=False, sensitive=()):
    env = os.environ.copy()
    env['MYSQL_PWD'] = password
    args = ['mysql', '--protocol=socket', '--socket=/data/mysql.sock', '-uroot', '--batch']
    if expired:
        args.append('--connect-expired-password')
    result = subprocess.run(args, input=sql.encode(), env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    sanitized_log(result.stdout, (password, secret('db-password'), *sensitive))
    if result.returncode:
        fail('Database bootstrap failed; protected diagnostic log retained.')

def artisan(env, *args):
    result = subprocess.run(['php', 'artisan', *args, '--no-ansi'], env=env,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    sanitized_log(result.stdout, (env['APP_KEY'], env['DB_PASSWORD'], env['SOCKETCLUSTER_AUTH_KEY']))
    if result.returncode:
        fail('Application initialization failed; protected diagnostic log retained.')

def start_mysql():
    process = subprocess.Popen(mysql_args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
    children.append(process)
    for _ in range(90):
        if stopping or process.poll() is not None:
            fail('Database exited during startup.')
        try:
            with socket.socket(socket.AF_UNIX) as probe:
                probe.settimeout(1)
                probe.connect('/data/mysql.sock')
                return
        except OSError:
            time.sleep(1)
    fail('Database readiness timed out.')

def start_redis(env):
    directory = state / 'redis'
    if directory.is_symlink() or not directory.is_dir() or not directory.resolve().is_relative_to(state.resolve()):
        fail('Redis state is missing or invalid; refusing to start.')
    redis_env = env.copy()
    redis_env['LD_LIBRARY_PATH'] = '/opt/redis-libs'
    args = ['/opt/redis/redis-server', '--bind', '127.0.0.1', '--port', '6379',
            '--dir', '/data/fleetbase/redis', '--appendonly', 'yes', '--appendfsync', 'always',
            '--save', '', '--protected-mode', 'yes', '--loglevel', 'warning']
    process = subprocess.Popen(args, env=redis_env, start_new_session=True)
    children.append(process)
    for _ in range(60):
        if stopping or process.poll() is not None:
            fail('Redis exited during startup.')
        try:
            with socket.create_connection(('127.0.0.1', 6379), timeout=1) as probe:
                probe.sendall(b'*1\r\n$4\r\nPING\r\n')
                if probe.recv(16) == b'+PONG\r\n':
                    return
        except OSError:
            pass
        time.sleep(1)
    fail('Redis readiness timed out.')


def stop_children(clean):
    # The active-run marker was persisted before starting MySQL; leave it in
    # place after any failure or forced termination.
    marker = private / 'unclean'
    forced = False
    # Drain writers before stopping their database, with a bounded total wait.
    # Stop application writers first, then Redis, then MySQL.
    for group in (list(reversed(children[2:])), children[1:2], children[:1]):
        deadline = time.monotonic() + 4
        for proc in group:
            if proc.poll() is None:
                try:
                    os.killpg(proc.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
        for proc in group:
            try:
                proc.wait(timeout=max(0, deadline - time.monotonic()))
            except subprocess.TimeoutExpired:
                forced = True
                try:
                    os.killpg(proc.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                proc.wait()
    if clean and not forced and marker.is_file():
        marker.unlink()
        sync_private_directory()


try:
    if not root.is_dir():
        fail('Persistent state is missing.')
    initialized = private / 'initialized'
    first = not private.exists()
    if first:
        if any(root.iterdir()):
            fail('Unknown existing state; refusing initialization.')
        private.mkdir(mode=0o700)
        (private / 'initializing').write_text('incomplete')
        state.mkdir(mode=0o700)
        for name in ('storage', 'cache'):
            shutil.copytree('/opt/ato-seed/' + name, state / name)
        for name in ('home', 'tmp', 'config', 'console', 'redis'):
            (state / name).mkdir(mode=0o700)
        app_key = 'base64:' + base64.b64encode(secrets.token_bytes(32)).decode()
        secret('app-key', app_key)
        secret('db-password', secrets.token_hex(32))
        secret('socket-key', secrets.token_hex(32))
        result = subprocess.run(mysql_args[:-1] + ['--initialize'], stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        if result.returncode:
            fail('Database initialization failed; refusing to reset state.')
        initlog = result.stdout.decode('utf-8', errors='replace')
    elif not initialized.is_file():
        fail('Incomplete or unknown initialization; refusing to reset state.')
    if (private / 'unclean').exists():
        fail('Previous shutdown was unclean; refusing automatic restart.')
    mark_active()
    # A cached Laravel config would freeze host Bindings and keep credentials
    # in generated PHP. Re-evaluate the current environment at each start.
    config_cache = state / 'cache' / 'config.php'
    if config_cache.is_symlink() or not config_cache.parent.resolve().is_relative_to(state.resolve()):
        fail('Unexpected configuration-cache path.')
    config_cache.unlink(missing_ok=True)
    app_key = secret('app-key')
    password = secret('db-password')
    socket_key = secret('socket-key')
    env = os.environ.copy()
    env.update({'HOME':'/data/fleetbase/home', 'TMPDIR':'/data/fleetbase/tmp',
                'XDG_CONFIG_HOME':'/data/fleetbase/config', 'XDG_DATA_HOME':'/data/fleetbase/home',
                'APP_ENV':'production', 'APP_DEBUG':'false', 'APP_KEY':app_key,
                'DB_HOST':'localhost', 'DB_DATABASE':'fleetbase', 'DB_USERNAME':'fleetbase',
                'DB_PASSWORD':password, 'LOG_CHANNEL':'stderr',
                'REDIS_URL':'tcp://127.0.0.1:6379', 'CACHE_URL':'tcp://127.0.0.1:6379',
                'SOCKETCLUSTER_AUTH_ENABLED':'true', 'SOCKETCLUSTER_AUTH_MODE':'enforce',
                'SOCKETCLUSTER_AUTH_KEY':socket_key, 'SOCKETCLUSTER_PUBLISH_URL':'http://127.0.0.1:8003'})
    env.pop('DATABASE_URL', None)
    start_mysql()
    start_redis(env)
    if first:
        match = re.search(r'temporary password is generated for root@localhost:\s*(\S+)', initlog, re.I)
        if not match:
            fail('Initial database credential was not generated.')
        root_password = secrets.token_hex(32)
        sql = f"ALTER USER 'root'@'localhost' IDENTIFIED BY '{root_password}';\n"
        for db in ('fleetbase', 'fleetbase_sandbox', 'fleetbase_storefront'):
            sql += f'CREATE DATABASE `{db}` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;\n'
        sql += f"CREATE USER 'fleetbase'@'localhost' IDENTIFIED BY '{password}';\n"
        for db in ('fleetbase', 'fleetbase_sandbox', 'fleetbase_storefront'):
            sql += f"GRANT ALL PRIVILEGES ON `{db}`.* TO 'fleetbase'@'localhost';\n"
        mysql(match.group(1), sql, expired=True, sensitive=(root_password,))
        for args in (('mysql:createdb',), ('migrate','--force'), ('sandbox:migrate','--force'),
                     ('fleetbase:seed',), ('fleetbase:create-permissions',), ('queue:restart',),
                     ('schedule-monitor:sync',), ('cache:clear',), ('route:clear',),
                     ('route:cache',), ('registry:init',)):
            artisan(env, *args)
        initialized.write_text('initialized')
        (private / 'initializing').unlink()
    socket_env = env.copy()
    socket_env.update({'LD_LIBRARY_PATH':'/opt/socket-libs', 'SOCKETCLUSTER_PORT':'8002',
                       'SOCKETCLUSTER_INTERNAL_PORT':'8003',
                       'SOCKETCLUSTER_AUTHORIZE_URL':'http://127.0.0.1:8000/int/v1/socket/authorize'})
    children.append(subprocess.Popen(('/opt/fleetbase-socket/node', 'server.js'),
                                     cwd='/opt/fleetbase-socket', env=socket_env, start_new_session=True))
    for args in (('php','artisan','queue:work'), ('php','artisan','schedule:work'),
                 ('php','artisan','octane:frankenphp','--max-requests=1000','--port=8000','--host=0.0.0.0')):
        children.append(subprocess.Popen(args, env=env, start_new_session=True))
    while not stopping:
        if any(child.poll() is not None for child in children):
            fail('A required Fleetbase process exited.')
        # As container PID 1, collect orphaned grandchildren too. If a
        # required direct child wins the race against poll(), record its exit.
        while True:
            try:
                pid, status = os.waitpid(-1, os.WNOHANG)
            except ChildProcessError:
                break
            if pid == 0:
                break
            required = next((proc for proc in children if proc.pid == pid), None)
            if required is not None:
                required.returncode = os.waitstatus_to_exitcode(status)
                fail('A required Fleetbase process exited.')
        time.sleep(1)
    clean_shutdown = True
except Exception as error:
    if private.is_dir() and (private / 'initialized').is_file():
        (private / 'unclean').write_text('a required process exited unexpectedly')
    print('Fleetbase local prototype stopped:', type(error).__name__, file=sys.stderr, flush=True)
    sys.exit(1)
finally:
    stop_children(clean_shutdown)
