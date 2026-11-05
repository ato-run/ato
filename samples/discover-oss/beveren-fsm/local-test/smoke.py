import os
import re
import requests

base = 'http://web:8000'
session = requests.Session()
session.trust_env = False
route = base + '/api/resource/Service%20Request?limit_page_length=1'
anonymous = session.get(route, timeout=15)
assert anonymous.status_code in (401, 403), anonymous.status_code
login = session.post(base + '/api/method/login', data={
    'usr': 'Administrator', 'pwd': os.environ['BEVEREN_ADMIN_PASSWORD'],
}, timeout=20)
assert login.status_code == 200, login.status_code
user = session.get(base + '/api/method/frappe.auth.get_logged_user', timeout=15)
assert user.status_code == 200 and user.json()['message'] == 'Administrator'
listing = session.get(route, timeout=15)
assert listing.status_code == 200 and isinstance(listing.json()['data'], list)
page = session.get(base + '/schedule', timeout=15)
assert page.status_code == 200, page.status_code
match = re.search(r"window\.csrf_token\s*=\s*'([^']+)'", page.text)
assert match and match.group(1) != '{{ csrf_token }}'
asset = re.search(r'(?:src|href)="(/assets/beveren_fsm/schedule/assets/index-[^"]+\.js)"', page.text)
assert asset, 'Frontend script reference missing'
js = session.get(base + asset.group(1), timeout=15)
assert js.status_code == 200 and len(js.content) > 100_000, (js.status_code, len(js.content))
print('Anonymous access rejected; synthetic admin login, service-request listing, scheduling HTML and JS asset succeeded.')
for resource in ['Company', 'Customer', 'Customer Group', 'Territory', 'Service Type', 'Currency', 'Item Group', 'UOM', 'Price List']:
    response = session.get(base + '/api/resource/' + requests.utils.quote(resource, safe=''),
                           params={'fields': '["name"]', 'limit_page_length': 5}, timeout=15)
    assert response.status_code == 200, (resource, response.status_code)
    print(resource, 'examples:', [item['name'] for item in response.json()['data']])
