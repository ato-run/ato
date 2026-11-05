"""Exercise unassigned-order authorization against an isolated synthetic site."""
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
body = {'naming_series': 'SVC-ORD-.YYYY.-', 'customer': 'Synthetic Customer',
    'company': 'Synthetic Field Company', 'due_date': '2026-12-01', 'posting_date': '2026-11-06',
    'priority': 'Medium', 'type': 'Service', 'status': 'Open', 'plc_conversion_rate': 1,
    'price_list_currency': 'USD', 'selling_price_list': 'Standard Selling', 'currency': 'USD',
    'conversion_rate': 1, 'grand_total': 100, 'base_grand_total': 100,
    'items': [{'item_code': 'SYNTHETIC-SERVICE', 'item_name': 'Synthetic Service',
               'qty': 1, 'uom': 'Nos', 'rate': 100, 'amount': 100}]}
result = admin.post(base + '/api/resource/Service%20Order', json=body, timeout=30)
assert result.status_code == 200, (result.status_code, result.json().get('exception'))
doc = result.json()['data']
result = admin.post(base + '/api/method/frappe.client.submit', json={'doc': doc}, timeout=30)
assert result.status_code == 200, (result.status_code, result.json().get('exception'))
name = doc['name']
email = 'synthetic-schedule-' + secrets.token_hex(5) + '@example.invalid'
password = secrets.token_urlsafe(32)
result = admin.post(base + '/api/resource/User', json={'email': email, 'first_name': 'Synthetic Schedule Viewer',
    'user_type': 'Website User', 'enabled': 1, 'send_welcome_email': 0, 'new_password': password}, timeout=30)
assert result.status_code == 200, (result.status_code, result.json().get('exception'))
viewer = requests.Session()
viewer.trust_env = False
assert viewer.post(base + '/api/method/login', data={'usr': email, 'pwd': password}, timeout=20).status_code == 200
ordinary = viewer.get(base + '/api/resource/Service%20Order/' + name, timeout=20)
url = base + '/api/method/beveren_fsm.field_service_management.api.schedule.get_unassigned_service_orders'
result = viewer.get(url, timeout=20)
visible = result.status_code == 200 and any(x['name'] == name for x in result.json().get('message', []))
print('Restricted order HTTP', ordinary.status_code, 'schedule list HTTP', result.status_code, 'order visible:', visible)
assert ordinary.status_code == 403 and not visible, 'Unassigned list leaks restricted order'
result = admin.get(url, timeout=20)
assert result.status_code == 200 and any(x['name'] == name for x in result.json()['message'])
shared = admin.post(base + '/api/method/frappe.share.add', json={'doctype': 'Service Order', 'name': name,
    'user': email, 'read': 1, 'notify': 0}, timeout=20)
assert shared.status_code == 200, (shared.status_code, shared.json().get('exc_type'))
result = viewer.get(url, timeout=20)
assert result.status_code == 200 and any(x['name'] == name for x in result.json()['message'])
print('Administrator and explicitly shared user could see the unassigned order.')
