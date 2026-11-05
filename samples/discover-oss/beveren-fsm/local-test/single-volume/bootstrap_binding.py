#!/usr/bin/env python3
"""Local-only bootstrap candidate for an isolated synthetic Frappe site.

The container's privileged database socket user must exist before calling this
script. A real runner integration needs supervision, crash recovery, review,
and an owner-controlled secret binding before it can use this candidate.
"""
import os
import re
import secrets
import sys
import tempfile
from contextlib import redirect_stderr, redirect_stdout


def main():
    if len(sys.argv) != 2 or not re.fullmatch(r'[a-z0-9][a-z0-9.-]{0,61}[a-z0-9]', sys.argv[1]):
        raise ValueError('A valid synthetic site name is required')
    site = sys.argv[1]
    if '..' in site:
        raise ValueError('Invalid site name')
    password = os.environ.pop('ATO_BINDING_INITIAL_ADMIN_PASSWORD', None)
    if not password or not 16 <= len(password) <= 256 or '\x00' in password:
        raise ValueError('An initial administrator binding of 16–256 characters is required')

    os.umask(0o077)
    os.environ['FRAPPE_BENCH_ROOT'] = '/data/bench'
    os.chdir('/data/bench/sites')
    import frappe
    import pymysql
    from frappe.installer import _new_site
    from frappe.utils.password import check_password

    if os.path.lexists(site):
        raise FileExistsError('Site already exists')
    bootstrap_user = 'ato_setup_' + secrets.token_hex(6)
    bootstrap_password = secrets.token_hex(32)
    connection = pymysql.connect(unix_socket='/data/mysql.sock', user='frappe', autocommit=True)
    try:
        with connection.cursor() as cursor:
            cursor.execute("CREATE USER %s@'localhost' IDENTIFIED BY %s", (bootstrap_user, bootstrap_password))
            cursor.execute("GRANT ALL PRIVILEGES ON *.* TO %s@'localhost' WITH GRANT OPTION", (bootstrap_user,))
        with tempfile.TemporaryFile(mode='w+t', dir='/data/tmp') as captured:
            with redirect_stdout(captured), redirect_stderr(captured):
                frappe.init(site=site, new_site=True)
                _new_site(None, site, db_root_username=bootstrap_user, db_root_password=bootstrap_password,
                          admin_password=password, db_type='mariadb', db_socket='/data/mysql.sock',
                          install_apps=('erpnext', 'beveren_fsm'))
            frappe.destroy()
            frappe.init(site=site, sites_path='/data/bench/sites')
            frappe.connect()
            if check_password('Administrator', password) != 'Administrator':
                raise RuntimeError('Administrator verification failed')
            if not {'frappe', 'erpnext', 'beveren_fsm'} <= set(frappe.get_installed_apps()):
                raise RuntimeError('Application installation verification failed')
            frappe.destroy()
            captured.seek(0)
            output = captured.read()
            if password in output or bootstrap_password in output:
                raise RuntimeError('Installer output contained a credential')
    finally:
        with connection.cursor() as cursor:
            cursor.execute("DROP USER IF EXISTS %s@'localhost'", (bootstrap_user,))
        connection.close()
    print('Local synthetic site initialized and administrator verified.')


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        # Third-party exception details may include credentials; do not emit them.
        print(f'Local synthetic bootstrap failed: {type(error).__name__}', file=sys.stderr)
        raise SystemExit(1) from None
