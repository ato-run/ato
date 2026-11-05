"""Synthetic Frappe bootstrap without placing credentials in process arguments."""
import contextlib
import os
import secrets

os.umask(0o077)
os.environ['FRAPPE_BENCH_ROOT'] = '/data/bench'
os.chdir('/data/bench/sites')
import frappe
import pymysql
from frappe.installer import _new_site
from frappe.utils.password import check_password

site = 'private-bootstrap.local'
admin_password = secrets.token_urlsafe(32)
db_password = secrets.token_hex(32)
bootstrap = 'ato_probe_' + secrets.token_hex(4)
connection = pymysql.connect(unix_socket='/data/mysql.sock', user='frappe', autocommit=True)
try:
    with connection.cursor() as cursor:
        cursor.execute("CREATE USER %s@'localhost' IDENTIFIED BY %s", (bootstrap, db_password))
        cursor.execute("GRANT ALL PRIVILEGES ON *.* TO %s@'localhost' WITH GRANT OPTION", (bootstrap,))
    with open('/data/tmp/private-bootstrap.log', 'w') as log:
        with contextlib.redirect_stdout(log), contextlib.redirect_stderr(log):
            frappe.init(site=site, new_site=True)
            _new_site(None, site, db_root_username=bootstrap, db_root_password=db_password,
                      admin_password=admin_password, db_type='mariadb', db_socket='/data/mysql.sock',
                      install_apps=('erpnext', 'beveren_fsm'))
    frappe.destroy()
    frappe.init(site=site, sites_path='/data/bench/sites')
    frappe.connect()
    assert check_password('Administrator', admin_password) == 'Administrator'
    apps = set(frappe.get_installed_apps())
    assert {'frappe', 'erpnext', 'beveren_fsm'} <= apps
    frappe.destroy()
    with open('/data/tmp/private-bootstrap.log') as log:
        output = log.read()
        assert admin_password not in output and db_password not in output
    print('Synthetic Frappe site installed; admin credential verified without process arguments or log disclosure.')
finally:
    with connection.cursor() as cursor:
        cursor.execute("DROP USER IF EXISTS %s@'localhost'", (bootstrap,))
    connection.close()
