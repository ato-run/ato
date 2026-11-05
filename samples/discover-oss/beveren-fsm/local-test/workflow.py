import json
import os
import re
import requests

base = 'http://web:8000'
s = requests.Session()
s.trust_env = False
assert s.post(base + '/api/method/login', data={'usr': 'Administrator',
    'pwd': os.environ['BEVEREN_ADMIN_PASSWORD']}, timeout=20).status_code == 200
page = s.get(base + '/schedule', timeout=20)
token = re.search(r"window\.csrf_token\s*=\s*'([^']+)'", page.text)
assert page.status_code == 200 and token
s.headers['X-Frappe-CSRF-Token'] = token.group(1)

def find(doctype, field, value):
    response = s.get(base + '/api/resource/' + requests.utils.quote(doctype, safe=''),
                     params={'filters': json.dumps([[field, '=', value]]), 'fields': '["name"]'}, timeout=20)
    assert response.status_code == 200, (doctype, response.status_code)
    data = response.json()['data']
    assert len(data) <= 1
    return data[0]['name'] if data else None

def create(doctype, body):
    response = s.post(base + '/api/resource/' + requests.utils.quote(doctype, safe=''),
                      json=body, timeout=30)
    if response.status_code not in (200, 201):
        print('Create failed:', doctype, response.status_code, response.json().get('exc_type', 'unknown'),
              response.json().get('exception', ''))
        print('Trace:', '\n'.join(json.loads(response.json().get('exc', '[]')))[-1800:])
        raise SystemExit(1)
    return response.json()['data']['name']

customer = find('Customer', 'customer_name', 'Synthetic Customer')
if not customer:
    customer = create('Customer', {'customer_name': 'Synthetic Customer', 'customer_type': 'Individual',
                                   'customer_group': 'Individual', 'territory': 'United States'})
subject = 'Synthetic maintenance request'
request = find('Service Request', 'subject', subject)
if not request:
    request = create('Service Request', {'subject': subject, 'priority': 'Medium',
        'due_date': '2026-12-01', 'posting_date': '2026-11-06', 'customer': customer,
        'naming_series': 'SR-.YYYY.-', 'company': 'Synthetic Field Company',
        'type': 'Service', 'currency': 'USD', 'status': 'Open'})
response = s.get(base + '/api/method/beveren_fsm.field_service_management.api.service_request.get_service_requests',
                 params={'search': customer}, timeout=20)
assert response.status_code == 200
matches = [item for item in response.json()['message'] if item['name'] == request]
assert len(matches) == 1 and matches[0]['subject'] == subject and matches[0]['status'] == 'Open'
print('Synthetic customer and service request created and retrieved through Beveren API:', request)
item = find('Item', 'item_code', 'SYNTHETIC-SERVICE')
if not item:
    item = create('Item', {'item_code': 'SYNTHETIC-SERVICE', 'item_name': 'Synthetic Service',
                           'item_group': 'Services', 'stock_uom': 'Nos', 'is_stock_item': 0})
order = find('Service Order', 'service_request', request)
if not order:
    order = create('Service Order', {'naming_series': 'SVC-ORD-.YYYY.-',
        'customer': customer, 'company': 'Synthetic Field Company', 'service_request': request,
        'due_date': '2026-12-01', 'posting_date': '2026-11-06', 'priority': 'Medium',
        'type': 'Service', 'status': 'Open', 'plc_conversion_rate': 1,
        'price_list_currency': 'USD', 'selling_price_list': 'Standard Selling',
        'currency': 'USD', 'conversion_rate': 1, 'grand_total': 100, 'base_grand_total': 100,
        'items': [{'item_code': item, 'item_name': 'Synthetic Service', 'qty': 1,
                   'uom': 'Nos', 'rate': 100, 'amount': 100}]})
response = s.get(base + '/api/method/beveren_fsm.field_service_management.api.service_order.get_service_orders_for_tracking',
                 params={'search': customer}, timeout=20)
assert response.status_code == 200
matches = [entry for entry in response.json()['message'] if entry['name'] == order]
assert len(matches) == 1 and matches[0]['customer'] == customer
print('Synthetic service order created and retrieved through Beveren API:', order)
order_url = base + '/api/resource/Service%20Order/' + requests.utils.quote(order, safe='')
current = s.get(order_url, timeout=20)
assert current.status_code == 200
movements = current.json()['data'].get('product_movement', [])
if not any(m.get('movement_type') == 'Deliver to Customer' for m in movements):
    response = s.post(base + '/api/method/beveren_fsm.field_service_management.doctype.service_order.service_order.record_product_movement',
                      json={'service_order': order, 'product_location': 'Deliver to Customer'}, timeout=30)
    assert response.status_code == 200, (response.status_code, response.json().get('exc_type'))
current = s.get(order_url, timeout=20)
assert current.status_code == 200
assert any(m.get('movement_type') == 'Deliver to Customer'
           for m in current.json()['data'].get('product_movement', []))
print('Authorized synthetic order movement saved and reread.')
