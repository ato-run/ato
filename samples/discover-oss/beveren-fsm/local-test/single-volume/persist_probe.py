"""Upload and re-read only synthetic data in the isolated local experiment."""
import os
import re
import sys
import requests

base = 'http://127.0.0.1:8000'
session = requests.Session()
session.trust_env = False
result = session.post(base + '/api/method/login', data={'usr': 'Administrator',
    'pwd': os.environ['BEVEREN_ADMIN_PASSWORD']}, timeout=30)
assert result.status_code == 200, ('login', result.status_code)
page = session.get(base + '/schedule', timeout=30)
token = re.search(r"window\.csrf_token\s*=\s*'([^']+)'", page.text)
assert page.status_code == 200 and token
session.headers['X-Frappe-CSRF-Token'] = token.group(1)
marker = b'synthetic beveren persisted attachment\n'
if sys.argv[1] == 'create':
    result = session.post(base + '/api/method/upload_file', data={'is_private': '1'},
        files={'file': ('synthetic-attachment.txt', marker, 'text/plain')}, timeout=30)
    assert result.status_code == 200, ('upload', result.status_code)
    path = result.json()['message']['file_url']
    assert path.startswith('/private/files/') and '..' not in path
    with open('/data/tmp/synthetic-file-url', 'w') as output:
        output.write(path)
    print('Uploaded synthetic private attachment.')
elif sys.argv[1] == 'verify':
    for doctype, name in [('Service Request', 'SR-2026-00001'), ('Service Order', 'SVC-ORD-2026-00001')]:
        result = session.get(base + '/api/resource/' + requests.utils.quote(doctype, safe='') + '/' + name,
                             timeout=30)
        assert result.status_code == 200, (doctype, result.status_code)
        assert result.json()['data']['name'] == name
    with open('/data/tmp/synthetic-file-url') as input_file:
        path = input_file.read()
    assert path.startswith('/private/files/') and '..' not in path
    result = session.get(base + path, timeout=30)
    assert result.status_code == 200 and result.content == marker, ('file', result.status_code)
    print('Synthetic request, order and private attachment survived restart.')
else:
    raise ValueError('Expected create or verify')
