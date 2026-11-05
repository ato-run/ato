"""Read synthetic records from the separately restored local Frappe site."""
import os
import requests

base = 'http://beveren-fsm-proof-restored:8001'
session = requests.Session()
session.trust_env = False
result = session.post(base + '/api/method/login', data={'usr': 'Administrator',
    'pwd': os.environ['BEVEREN_ADMIN_PASSWORD']}, timeout=20)
assert result.status_code == 200, ('login', result.status_code)
for doctype, name in [('Service Request', 'SR-2026-00001'), ('Service Order', 'SVC-ORD-2026-00001')]:
    result = session.get(base + '/api/resource/' + requests.utils.quote(doctype, safe='') + '/' + name,
                         timeout=20)
    assert result.status_code == 200, (doctype, result.status_code)
    assert result.json()['data']['name'] == name
    print('Restored synthetic record readable:', doctype, name)
