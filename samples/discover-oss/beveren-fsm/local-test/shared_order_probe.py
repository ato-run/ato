import os
import re
import secrets
import requests

base = 'http://web:8000'
method = '/api/method/beveren_fsm.field_service_management.doctype.service_order.service_order.record_product_movement'
order = 'SVC-ORD-2026-00001'
request = 'SR-2026-00001'

def login(user, password):
    session = requests.Session()
    session.trust_env = False
    assert session.post(base + '/api/method/login', data={'usr': user, 'pwd': password}, timeout=20).status_code == 200
    page = session.get(base + '/schedule', timeout=20)
    token = re.search(r"window\.csrf_token\s*=\s*'([^']+)'", page.text)
    assert page.status_code == 200 and token
    session.headers['X-Frappe-CSRF-Token'] = token.group(1)
    return session

admin = login('Administrator', os.environ['BEVEREN_ADMIN_PASSWORD'])
email = 'synthetic-shared-' + secrets.token_hex(6) + '@example.invalid'
password = secrets.token_urlsafe(32)
r = admin.post(base + '/api/resource/User', json={'email': email, 'first_name': 'Synthetic Shared',
    'user_type': 'Website User', 'enabled': 1, 'send_welcome_email': 0, 'new_password': password}, timeout=30)
assert r.status_code in (200, 201), (r.status_code, r.json().get('exc_type'))

def share(doctype, name):
    r = admin.post(base + '/api/method/frappe.share.add', json={'doctype': doctype, 'name': name,
        'user': email, 'read': 1, 'write': 1, 'notify': 0}, timeout=20)
    assert r.status_code == 200, (doctype, r.status_code, r.json().get('exc_type'))

def count():
    r = admin.get(base + '/api/resource/Service%20Order/' + order, timeout=20)
    assert r.status_code == 200
    return len(r.json()['data'].get('product_movement', []))

share('Service Order', order)
viewer = login(email, password)
assert viewer.get(base + '/api/resource/Service%20Order/' + order, timeout=20).status_code == 200
assert viewer.get(base + '/api/resource/Service%20Request/' + request, timeout=20).status_code == 403
before = count()
response = viewer.post(base + method, json={'service_order': order, 'product_location': 'Closed'}, timeout=30)
assert response.status_code == 403, response.status_code
assert count() == before
share('Service Request', request)
requests_list = viewer.get(base + '/api/method/beveren_fsm.field_service_management.api.service_request.get_service_requests',
                           params={'search': 'Synthetic Customer'}, timeout=20)
orders_list = viewer.get(base + '/api/method/beveren_fsm.field_service_management.api.service_order.get_service_orders_for_tracking',
                         params={'search': 'Synthetic Customer'}, timeout=20)
assert requests_list.status_code == 200, requests_list.status_code
selected = [row for row in requests_list.json()['message'] if row.get('name') == request]
assert len(selected) == 1
assert any(row.get('service_order') == order for row in selected[0]['product_movement'])
assert orders_list.status_code == 200, orders_list.status_code
assert any(row.get('name') == order for row in orders_list.json()['message'])
response = viewer.post(base + method, json={'service_order': order, 'product_location': 'Closed'}, timeout=30)
assert response.status_code == 200, (response.status_code, response.json().get('exc_type'))
assert count() == before + 1
print('Order-only writer was refused without mutation; writer with both documents shared listed and updated them.')
