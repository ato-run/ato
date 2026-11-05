"""Exercise appointment authorization against an isolated synthetic site."""
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

def create(doctype, document):
    result = admin.post(base + '/api/resource/' + requests.utils.quote(doctype, safe=''),
                        json=document, timeout=30)
    assert result.status_code in (200, 201), (doctype, result.status_code, result.json().get('exception'))
    return result.json()['data']['name']

suffix = secrets.token_hex(4)
technician = create('Service Technician', {'full_name': 'Synthetic Technician ' + suffix})
appointment = create('Service Appointment', {
    'customer': 'Synthetic Customer', 'service_order': 'SVC-ORD-2026-00001',
    'posting_date': '2026-11-06', 'naming_series': 'SVC-APP-.YYYY.-',
    'scheduled_start_datetime': '2026-12-01 09:00:00',
    'scheduled_finish_datetime': '2026-12-01 10:00:00',
    'service_technicians': [{'service_technician': technician, 'full_name': 'Synthetic Technician ' + suffix}],
    'items': [{'item_code': 'SYNTHETIC-SERVICE', 'item_name': 'Synthetic Service',
               'qty': 1, 'uom': 'Nos', 'rate': 100, 'amount': 100}],
})
email = 'synthetic-appointment-viewer-' + suffix + '@example.invalid'
password = secrets.token_urlsafe(32)
create('User', {'email': email, 'first_name': 'Synthetic Viewer', 'user_type': 'Website User',
                'enabled': 1, 'send_welcome_email': 0, 'new_password': password})
viewer = requests.Session()
viewer.trust_env = False
assert viewer.post(base + '/api/method/login', data={'usr': email, 'pwd': password}, timeout=20).status_code == 200
ordinary = viewer.get(base + '/api/resource/Service%20Appointment/' + appointment, timeout=20)
prefix = base + '/api/method/beveren_fsm.field_service_management.api.service_appointment.'
one = viewer.get(prefix + 'get_appointment', params={'name': appointment}, timeout=20)
listing = viewer.get(prefix + 'get_appointments', timeout=20)
visible = listing.status_code == 200 and any(x.get('name') == appointment for x in listing.json().get('message', []))
print('Restricted user: standard appointment HTTP', ordinary.status_code,
      '; custom single HTTP', one.status_code, '; custom list HTTP', listing.status_code,
      '; unrelated appointment in list:', visible)
assert ordinary.status_code in (401, 403)
assert one.status_code in (401, 403) and not visible, 'Appointment API exposes a restricted document'
admin_one = admin.get(prefix + 'get_appointment', params={'name': appointment}, timeout=20)
admin_list = admin.get(prefix + 'get_appointments', timeout=20)
assert admin_one.status_code == 200 and admin_one.json()['message']['name'] == appointment
assert admin_list.status_code == 200 and any(x.get('name') == appointment for x in admin_list.json()['message'])
shared = admin.post(base + '/api/method/frappe.share.add', json={'doctype': 'Service Appointment',
    'name': appointment, 'user': email, 'read': 1, 'notify': 0}, timeout=20)
assert shared.status_code == 200, (shared.status_code, shared.json().get('exc_type'))
one = viewer.get(prefix + 'get_appointment', params={'name': appointment}, timeout=20)
listing = viewer.get(prefix + 'get_appointments', timeout=20)
assert one.status_code == 200 and one.json()['message']['name'] == appointment
assert listing.status_code == 200 and any(x.get('name') == appointment for x in listing.json()['message'])
print('Administrator and explicitly shared user could read the appointment through both endpoints.')
