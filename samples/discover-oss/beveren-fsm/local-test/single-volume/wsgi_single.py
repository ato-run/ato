"""Synthetic single-site Frappe app for an isolated Gunicorn experiment."""
import os

os.environ.setdefault('FRAPPE_BENCH_ROOT', '/data/bench')
os.chdir('/data/bench/sites')

import frappe.app  # noqa: E402 - Frappe must load after setting its bench root.

frappe.app._site = 'single2.local'
frappe.app._sites_path = '/data/bench/sites'
application = frappe.app.application_with_statics()
