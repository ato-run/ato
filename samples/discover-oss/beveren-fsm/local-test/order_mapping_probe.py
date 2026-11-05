"""Test source and target permissions for synthetic order mapping endpoints."""
import runpy
import secrets
import requests

ctx = runpy.run_path('sites/.tmp/authz_probe.py')
base, admin, viewer = ctx['base'], ctx['admin'], ctx['viewer']
email, order = ctx['email'], ctx['order_name']
prefix = base + '/api/method/beveren_fsm.field_service_management.doctype.service_order.service_order.'
methods = ('make_stock_entry', 'make_delivery_note', 'make_purchase_receipt',
           'make_purchase_order', 'make_purchase_invoice')
for method in methods:
    result = viewer.get(prefix + method, params={'service_order': order}, timeout=30)
    assert result.status_code == 403, (method, result.status_code)
share = admin.post(base + '/api/method/frappe.share.add', json={'doctype': 'Service Order',
    'name': order, 'user': email, 'read': 1, 'notify': 0}, timeout=20)
assert share.status_code == 200, share.status_code
for method in methods:
    result = viewer.get(prefix + method, params={'service_order': order}, timeout=30)
    assert result.status_code == 403, (method, result.status_code)
for method in ('make_purchase_order', 'make_purchase_invoice'):
    result = admin.get(prefix + method, params={'service_order': order}, timeout=30)
    assert result.status_code == 200, (method, result.status_code)
    assert result.json()['message']['custom_service_order'] == order
operator_email = 'synthetic-mapper-' + secrets.token_hex(6) + '@example.invalid'
password = secrets.token_urlsafe(32)
created = admin.post(base + '/api/resource/User', json={'email': operator_email,
    'first_name': 'Synthetic Mapper', 'user_type': 'System User', 'enabled': 1,
    'send_welcome_email': 0, 'new_password': password,
    'roles': [{'role': 'Purchase User'}, {'role': 'Accounts User'}]}, timeout=30)
assert created.status_code in (200, 201), (created.status_code, created.json().get('exc_type'))
share = admin.post(base + '/api/method/frappe.share.add', json={'doctype': 'Service Order',
    'name': order, 'user': operator_email, 'read': 1, 'notify': 0}, timeout=20)
assert share.status_code == 200, share.status_code
operator = requests.Session()
operator.trust_env = False
result = operator.post(base + '/api/method/login', data={'usr': operator_email, 'pwd': password}, timeout=20)
assert result.status_code == 200, result.status_code
for method in ('make_purchase_order', 'make_purchase_invoice'):
    result = operator.get(prefix + method, params={'service_order': order}, timeout=30)
    assert result.status_code == 200, (method, result.status_code, result.json().get('exc_type'))
    assert result.json()['message']['custom_service_order'] == order
print('Unshared and read-only users were refused; administrator and authorized operator could map the order.')
