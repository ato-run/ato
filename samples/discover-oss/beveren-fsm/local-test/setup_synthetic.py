import os
import re
import requests

base = 'http://web:8000'
s = requests.Session()
s.trust_env = False
r = s.post(base + '/api/method/login', data={
    'usr': 'Administrator', 'pwd': os.environ['BEVEREN_ADMIN_PASSWORD'],
}, timeout=20)
assert r.status_code == 200, r.status_code
page = s.get(base + '/schedule', timeout=20)
token = re.search(r"window\.csrf_token\s*=\s*'([^']+)'", page.text)
assert page.status_code == 200 and token
s.headers['X-Frappe-CSRF-Token'] = token.group(1)
args = {
    'currency': 'USD', 'full_name': 'Synthetic Operator',
    'company_name': 'Synthetic Field Company', 'company_abbr': 'SFC',
    'timezone': 'America/New_York', 'industry': 'Manufacturing',
    'country': 'United States', 'fy_start_date': '2026-01-01',
    'fy_end_date': '2026-12-31', 'language': 'english',
    'company_tagline': 'Synthetic test data', 'email': 'operator@example.invalid',
    'password': os.environ['BEVEREN_ADMIN_PASSWORD'], 'chart_of_accounts': 'Standard',
}
r = s.post(base + '/api/method/frappe.desk.page.setup_wizard.setup_wizard.setup_complete',
           json={'args': args}, timeout=180)
if r.status_code != 200:
    print('Synthetic setup HTTP status:', r.status_code)
    print('Exception class:', r.json().get('exc_type', 'unknown'))
    raise SystemExit(1)
print('Synthetic setup HTTP status:', r.status_code, 'result:', r.json().get('message'))
