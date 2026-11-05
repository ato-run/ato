import os
import re
import secrets
import requests

base = 'http://web:8000'
admin = requests.Session()
admin.trust_env = False
assert admin.post(base + '/api/method/login', data={'usr': 'Administrator',
    'pwd': os.environ['BEVEREN_ADMIN_PASSWORD']}, timeout=20).status_code == 200
page = admin.get(base + '/schedule', timeout=20)
token = re.search(r"window\.csrf_token\s*=\s*'([^']+)'", page.text)
assert page.status_code == 200 and token
admin.headers['X-Frappe-CSRF-Token'] = token.group(1)
email = 'synthetic-viewer-' + secrets.token_hex(6) + '@example.invalid'
password = secrets.token_urlsafe(32)
created = admin.post(base + '/api/resource/User', json={
    'email': email, 'first_name': 'Synthetic Viewer', 'user_type': 'Website User',
    'enabled': 1, 'send_welcome_email': 0, 'new_password': password,
}, timeout=30)
assert created.status_code in (200, 201), ('create user', created.status_code, created.json().get('exc_type'))
viewer = requests.Session()
viewer.trust_env = False
assert viewer.post(base + '/api/method/login', data={'usr': email, 'pwd': password}, timeout=20).status_code == 200
ordinary = viewer.get(base + '/api/resource/Service%20Request', timeout=20)
custom = viewer.get(base + '/api/method/beveren_fsm.field_service_management.api.service_request.get_service_requests',
                    params={'search': 'Synthetic Customer'}, timeout=20)
orders = viewer.get(base + '/api/method/beveren_fsm.field_service_management.api.service_order.get_service_orders_for_tracking',
                    timeout=20)
found = custom.status_code == 200 and any(x.get('subject') == 'Synthetic maintenance request'
                                          for x in custom.json().get('message', []))
print('Restricted user: standard list HTTP', ordinary.status_code,
      '; Beveren request list HTTP', custom.status_code, '; unrelated request visible:', found,
      '; order list HTTP', orders.status_code)
if ordinary.status_code in (401, 403) and found:
    raise SystemExit(2)
assert orders.status_code in (401, 403), orders.status_code
order_name = 'SVC-ORD-2026-00001'
ordinary_order = viewer.get(base + '/api/resource/Service%20Order/' + order_name, timeout=20)
assert ordinary_order.status_code in (401, 403), ordinary_order.status_code
viewer_page = viewer.get(base + '/schedule', timeout=20)
viewer_token = re.search(r"window\.csrf_token\s*=\s*'([^']+)'", viewer_page.text)
assert viewer_page.status_code == 200 and viewer_token
viewer.headers['X-Frappe-CSRF-Token'] = viewer_token.group(1)
movement = viewer.post(base + '/api/method/beveren_fsm.field_service_management.doctype.service_order.service_order.record_product_movement',
                       json={'service_order': order_name, 'product_location': 'Receive From Customer'}, timeout=30)
print('Restricted user: standard order read HTTP', ordinary_order.status_code,
      '; record movement HTTP', movement.status_code)
assert movement.status_code in (401, 403), movement.status_code
