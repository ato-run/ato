"""Check that purchase mappings do not expose an unreadable service request."""
import runpy
import secrets

ctx = runpy.run_path('sites/.tmp/order_mapping_probe.py')
admin, operator = ctx['admin'], ctx['operator']
base, prefix, order, email = ctx['base'], ctx['prefix'], ctx['order'], ctx['operator_email']
request = 'SR-2026-00001'
supplier = 'Synthetic Private Vendor ' + secrets.token_hex(4)
result = admin.post(base + '/api/resource/Supplier', json={'supplier_name': supplier,
    'supplier_group': 'All Supplier Groups', 'supplier_type': 'Company'}, timeout=30)
assert result.status_code in (200, 201), (result.status_code, result.json().get('exception'))
supplier = result.json()['data']['name']
result = admin.put(base + '/api/resource/Service%20Request/' + request,
                   json={'repair_vendor': supplier}, timeout=30)
assert result.status_code == 200, (result.status_code, result.json().get('exception'))
assert operator.get(base + '/api/resource/Service%20Request/' + request, timeout=20).status_code == 403
for method in ('make_purchase_order', 'make_purchase_invoice'):
    result = operator.get(prefix + method, params={'service_order': order}, timeout=20)
    assert result.status_code == 200, (method, result.status_code)
    assert result.json()['message'].get('supplier') != supplier, 'Restricted request supplier was exposed'
shared = admin.post(base + '/api/method/frappe.share.add', json={'doctype': 'Service Request',
    'name': request, 'user': email, 'read': 1, 'notify': 0}, timeout=20)
assert shared.status_code == 200, shared.status_code
for method in ('make_purchase_order', 'make_purchase_invoice'):
    result = operator.get(prefix + method, params={'service_order': order}, timeout=20)
    assert result.status_code == 200 and result.json()['message'].get('supplier') == supplier
print('Purchase mappings hide the linked supplier until the request is shared.')
