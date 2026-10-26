#!/usr/bin/env python3
"""Read-only local Markdown target/fragment audit; no network or app execution."""
import argparse
import collections
import csv
import html
import json
import os
from pathlib import Path
import re
import subprocess
import unicodedata
from urllib.parse import unquote, urlsplit

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--ref', help='Read a Git snapshot instead of the working tree')
parser.add_argument('--output', required=True, help='JSON report path, relative to checkout')
parser.add_argument('--allow-historical-from', help='Allow only the exact historical failures in a reviewed audit report')
args = parser.parse_args()
root = Path(subprocess.check_output(['git', 'rev-parse', '--show-toplevel'], text=True).strip())
os.chdir(root)
if args.ref:
    files = set(subprocess.check_output(['git', 'ls-tree', '-r', '--name-only', '-z', args.ref]).decode().split('\0'))
else:
    files = set(subprocess.check_output(['git', 'ls-files', '--cached', '--others', '--exclude-standard', '-z']).decode().split('\0'))
files.discard('')
files = {p for p in files if not p.startswith('.tmp/') and (args.ref or Path(p).is_file())}

def read(path):
    if args.ref:
        return subprocess.check_output(['git', 'show', f'{args.ref}:{path}']).decode('utf-8', errors='replace')
    return Path(path).read_text(errors='replace')

def prose(text):
    """Exclude fenced code, including longer fences, and HTML comments."""
    result = []; fence = None
    for line in text.splitlines():
        m = re.match(r'^\s{0,3}(`{3,}|~{3,})', line)
        if m:
            token = m[1]
            if fence is None:
                fence = token
            elif token[0] == fence[0] and len(token) >= len(fence):
                fence = None
            result.append(''); continue
        result.append('' if fence else line)
    return re.sub(r'<!--.*?-->', '', '\n'.join(result), flags=re.S)

def anchors(text):
    result = set(re.findall(r'\b(?:id|name)=["\']([^"\']+)', text))
    counts = collections.Counter()
    for line in prose(text).splitlines():
        m = re.match(r'^#{1,6}\s+(.*?)\s*#*$', line)
        if not m: continue
        value = html.unescape(re.sub(r'<[^>]+>', '', m[1])).lower()
        value = re.sub(r'\[([^]]+)\]\([^)]+\)', r'\1', value)
        value = ''.join(c for c in value if c in '_-' or not unicodedata.category(c).startswith(('P', 'S')))
        value = value.replace(' ', '-')
        n = counts[value]; counts[value] += 1
        result.add(value + (f'-{n}' if n else ''))
    return result

inventory = Path('docs/archive/2026/documentation-reorganization/inventory.tsv')
historical = set()
if inventory.exists():
    for row in csv.DictReader(inventory.open(), delimiter='\t'):
        if row['category'] == 'Archive':
            historical.update([row['original_path'], row['destination']])

def scope(path):
    return 'historical' if path in historical or '/archive/' in path or '/archived/' in path else 'active'

texts = {p: read(p) for p in sorted(files) if p.endswith(('.md', '.mdx'))}
anchor_cache = {}
failures = []; checked = external = symbolic = 0
for src, raw in texts.items():
    text = prose(raw)
    links = re.findall(r'\]\((<[^>]+>|[^\s)]+)(?:\s+["\'][^\n]*?["\'])?\)', text)
    links += re.findall(r'^\s{0,3}\[[^]]+\]:\s*(<[^>]+>|\S+)', text, flags=re.M)
    for raw_url in links:
        url = html.unescape(raw_url.strip('<>'))
        u = urlsplit(url)
        if u.scheme or u.netloc: external += 1; continue
        if any(x in url for x in ['<', '>', '{', '}', '*']): symbolic += 1; continue
        if url.startswith('#/'):
            target = 'docs/' + unquote(url[2:]).split('?', 1)[0]
            if not Path(target).suffix: target += '.md'
            fragment = ''
        else:
            target = os.path.normpath(os.path.join(os.path.dirname(src), unquote(u.path))) if u.path else src
            if u.path.startswith('/'):
                # Docsify sidebars use absolute routes within the docs site.
                prefix = 'docs/' if src in ['docs/_sidebar.md', 'docs/rfcs/_sidebar.md'] else ''
                target = prefix + unquote(u.path).lstrip('/')
            fragment = unquote(u.fragment)
        checked += 1
        exists = target in files or any(p.startswith(target.rstrip('/') + '/') for p in files)
        reason = None
        if not exists: reason = 'missing target'
        elif fragment and target in texts:
            if target not in anchor_cache: anchor_cache[target] = anchors(texts[target])
            if fragment not in anchor_cache[target]: reason = 'missing fragment'
        if reason:
            failures.append({'source': src, 'url': url, 'target': target, 'reason': reason, 'scope': scope(src)})

report = {'snapshot': args.ref or 'working-tree', 'markdown_files': len(texts),
          'local_links_checked': checked, 'external_links_not_fetched': external,
          'symbolic_links_not_resolved': symbolic,
          'failures_by_scope': dict(collections.Counter(x['scope'] for x in failures)),
          'failures': failures,
          'limitations': ['External URLs are inventoried, not fetched.',
                         'Inline/reference Markdown paths and ATX/GitHub-style heading anchors are checked.',
                         'HTML application UI, rendered Docsify behavior, image OCR and inline code paths are not executed.']}
output = Path(args.output)
output.parent.mkdir(parents=True, exist_ok=True)
output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
print(json.dumps({k: v for k, v in report.items() if k not in ['failures', 'limitations']}, ensure_ascii=False))
if args.allow_historical_from:
    known = json.loads(Path(args.allow_historical_from).read_text())
    def failure_key(item):
        return (item['source'], item['url'], item['target'], item['reason'])
    allowed = collections.Counter(failure_key(item) for item in known['failures'] if item['scope'] == 'historical')
    remaining = allowed.copy()
    unexpected = []
    for item in failures:
        key = failure_key(item)
        if item['scope'] != 'historical' or remaining[key] == 0:
            unexpected.append(item)
        else:
            remaining[key] -= 1
    print(json.dumps({'reviewed_historical_failures': sum(allowed.values()), 'unexpected_failures': len(unexpected)}))
    raise SystemExit(1 if unexpected else 0)
raise SystemExit(1 if failures else 0)
