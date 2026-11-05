"""Exercise a synthetic appointment through Beveren's scheduling API."""
import re
import runpy
import secrets

ctx = runpy.run_path('sites/.tmp/schedule_probe.py')
base, admin, viewer, order = ctx['base'], ctx['admin'], ctx['viewer'], ctx['name']
suffix = secrets.token_hex(5)
technician_name = 'Synthetic Scheduling Technician ' + suffix
created = admin.post(base + '/api/resource/Service%20Technician',
    json={'full_name': technician_name}, timeout=30)
assert created.status_code in (200, 201), (created.status_code, created.json().get('exc_type'))
technician = created.json()['data']['name']
prefix = base + '/api/method/beveren_fsm.field_service_management.api.'
payload = {'posting_date': '2026-11-06', 'service_order': order, 'customer': 'Synthetic Customer',
    'scheduled_start_datetime': '2026-12-04 09:00:00',
    'scheduled_finish_datetime': '2026-12-04 10:00:00',
    'service_technicians': [{'service_technician': technician, 'full_name': technician_name}],
    'items': [{'item_code': 'SYNTHETIC-SERVICE', 'qty': 1, 'rate': 100, 'amount': 100}]}
created = admin.post(prefix + 'schedule.create_appointment_from_api', json=payload, timeout=30)
assert created.status_code == 200, (created.status_code, created.json().get('exception'))
appointment = created.json()['message']

def appointment_data():
    result = admin.get(prefix + 'service_appointment.get_appointment', params={'name': appointment}, timeout=20)
    assert result.status_code == 200, result.status_code
    return result.json()['message']

data = appointment_data()
assert data['service_order'] == order and data['status'] == 'Scheduled'
assert any(t['service_technician'] == technician for t in data['service_technicians'])
order_data = admin.get(base + '/api/resource/Service%20Order/' + order, timeout=20)
assert order_data.status_code == 200 and order_data.json()['data']['status'] == 'Scheduled'
viewer_page = viewer.get(base + '/schedule', timeout=20)
token = re.search(r"window\.csrf_token\s*=\s*'([^']+)'", viewer_page.text)
assert viewer_page.status_code == 200 and token
viewer.headers['X-Frappe-CSRF-Token'] = token.group(1)
update = {'name': appointment, 'scheduled_start_datetime': '2026-12-04 11:00:00',
          'scheduled_finish_datetime': '2026-12-04 12:00:00', 'service_technicians': [],
          'items': [], 'reschedule': True}
denied = viewer.post(prefix + 'schedule.update_appointment_from_api', json=update, timeout=30)
assert denied.status_code == 403, denied.status_code
assert appointment_data()['scheduled_start_datetime'].startswith('2026-12-04 09:00:00')
result = admin.post(prefix + 'schedule.update_appointment_from_api', json=update, timeout=30)
assert result.status_code == 200, (result.status_code, result.json().get('exception'))
assert appointment_data()['scheduled_start_datetime'].startswith('2026-12-04 11:00:00')
print('Scheduled and rescheduled a synthetic appointment; unauthorized change was rejected without mutation.')
