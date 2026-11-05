import base64, hashlib, re
from pathlib import Path
p=Path(__file__).parent
originals={'index.html':'20c9b8dbc349a51ef7c6fbd8a3820ee8b9687bd9475ded7ed73aa121f92cf401','assets/vendor.js':'33c92bdfb4f072854ceb9df12a815877ec57c061ad507c59718a62b7f5163009','assets/console.js':'6b2f8c0a64727078cbc5b6dd0699363cee8c97551dab0c974e7b777a71ba5f81'}
for name,digest in originals.items():
 if hashlib.sha256((p/name).read_bytes()).hexdigest()!=digest:raise ValueError('upstream asset digest mismatch: '+name)
html=(p/'index.html').read_text();vendor=(p/'assets/vendor.js').read_bytes();console=(p/'assets/console.js').read_bytes()
for ident in ('h','p'):
 pattern=(f'{ident}.default.API.host=`${{window.location.protocol}}//${{window.location.hostname}}`').encode();new=(f'{ident}.default.API.host=`${{window.location.protocol}}//${{window.location.host}}`').encode()
 if vendor.count(pattern)!=1:raise ValueError('unexpected API host fallback')
 vendor=vendor.replace(pattern,new)
module_start = vendor.index(b'define("@fleetbase/fleetops-data/models/vehicle"')
module_end = vendor.index(b'\ndefine(', module_start + 1)
module = vendor[module_start:module_end]
old_vehicle = b'at=(0,t.attr)("boolean"),nt=(0,t.attr)("raw")'
new_vehicle = b'at=(0,t.attr)("boolean",{defaultValue:!1}),nt=(0,t.attr)("raw")'
if module.count(old_vehicle) != 1:
    raise ValueError('unexpected vehicle model default')
vendor = vendor[:module_start] + module.replace(old_vehicle, new_vehicle) + vendor[module_end:]
old=b'socket:{path:"/socketcluster/",hostname:"",secure:!0,port:"38000"}'
new=b'socket:{path:"/socketcluster/",hostname:window.location.hostname,secure:window.location.protocol==="https:",port:window.location.port||(window.location.protocol==="https:"?"443":"80")}'
if console.count(old)!=1:raise ValueError('unexpected socket config')
console=console.replace(old,new)
# Explicit opt-in: the upstream form preselects external hosted routing.
old=b'k=I(f.prototype,"hostedRouting",[r.tracked],{configurable:!0,enumerable:!0,writable:!0,initializer:function(){return!0}})'
new=b'k=I(f.prototype,"hostedRouting",[r.tracked],{configurable:!0,enumerable:!0,writable:!0,initializer:function(){return!1}})'
if console.count(old)!=1:raise ValueError('unexpected hosted routing default')
console=console.replace(old,new)
for oldname,kind,data in [('vendor-2c099872119973d60d53830ac6654416.js','vendor',vendor),('@fleetbase/console-dbc4c1a34591f66d1730612f9c1cdae7.js','console',console)]:
 digest=hashlib.sha256(data).hexdigest();newname=f'ato-{kind}-{digest}.js';(p/'assets'/newname).write_bytes(data)
 sri='sha256-'+base64.b64encode(hashlib.sha256(data).digest()).decode()+' sha512-'+base64.b64encode(hashlib.sha512(data).digest()).decode()
 pattern=r'(<script src="/assets/)'+re.escape(oldname)+r'(" integrity=")[^"]+'
 html,count=re.subn(pattern,lambda m:m.group(1)+newname+m.group(2)+sri,html)
 if count!=1:raise ValueError('unexpected HTML script tag '+oldname)
(p/'index-ato.html').write_text(html)
print('Patched two API fallbacks and same-origin socket configuration and routing opt-in and vehicle default; recomputed SRI and names.')
