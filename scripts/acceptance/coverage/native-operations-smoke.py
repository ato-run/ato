#!/usr/bin/env python3
"""Offline operation integration fixtures, distinct from Runtime acceptance."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import uuid
import zipfile

REPO = Path(__file__).resolve().parents[3]
HELPERS = REPO / 'lib/formation/src/proposal'


def sha(path):
    return 'sha256:' + hashlib.sha256(path.read_bytes()).hexdigest()


def wheel(path, name, module, source):
    dist = f'{name}-1.0.0.dist-info'
    with zipfile.ZipFile(path, 'w') as archive:
        archive.writestr(module + '.py', source)
        archive.writestr(dist + '/METADATA', f'Metadata-Version: 2.1\nName: {name}\nVersion: 1.0.0\n')
        archive.writestr(dist + '/WHEEL', 'Wheel-Version: 1.0\nGenerator: fixture\nRoot-Is-Purelib: true\nTag: py3-none-any\n')
        archive.writestr(dist + '/RECORD', '')


def python_case(root):
    source = root / 'python'; source.mkdir()
    backend = source / 'ato_fixture_backend-1.0.0-py3-none-any.whl'
    # The fixture builds a real wheel through pip/PEP 517, using only stdlib.
    backend_code = '''import pathlib, zipfile
def get_requires_for_build_wheel(config_settings=None): return []
def prepare_metadata_for_build_wheel(metadata_directory, config_settings=None):
 d=pathlib.Path(metadata_directory)/'ato_fixture_app-1.0.0.dist-info';d.mkdir()
 (d/'METADATA').write_text('Metadata-Version: 2.1\\nName: ato-fixture-app\\nVersion: 1.0.0\\n')
 return d.name
def build_wheel(wheel_directory, config_settings=None, metadata_directory=None):
 name='ato_fixture_app-1.0.0-py3-none-any.whl';dist='ato_fixture_app-1.0.0.dist-info'
 with zipfile.ZipFile(pathlib.Path(wheel_directory)/name,'w') as z:
  z.writestr('ato_fixture_app.py','VALUE = 42\\n')
  z.writestr(dist+'/METADATA','Metadata-Version: 2.1\\nName: ato-fixture-app\\nVersion: 1.0.0\\n')
  z.writestr(dist+'/WHEEL','Wheel-Version: 1.0\\nRoot-Is-Purelib: true\\nTag: py3-none-any\\n')
  z.writestr(dist+'/RECORD','')
 return name
'''
    wheel(backend, 'ato_fixture_backend', 'ato_fixture_backend', backend_code)
    sdist = source / 'ato_fixture_app-1.0.0.tar.gz'
    pyproject = b'[build-system]\nrequires=["ato-fixture-backend==1.0.0"]\nbuild-backend="ato_fixture_backend"\n'
    with tarfile.open(sdist, 'w:gz') as archive:
        info = tarfile.TarInfo('ato_fixture_app-1.0.0/pyproject.toml'); info.size = len(pyproject)
        archive.addfile(info, io.BytesIO(pyproject))
    requirements = source / 'requirements.txt'; requirements.write_text(sdist.as_uri() + '\n')
    plan = dict(schema='ato.python-build-plan/1', python_version=sys.version.split()[0], root=str(source / 'operation'), requirements=str(requirements),
                requirements_sha256=sha(requirements), build_dependencies=[dict(name='ato-fixture-backend', version='1.0.0')],
                toolchains=[], build_network='denied', lock_operation=(HELPERS / 'python-dependency-lock.py').read_text())
    environment = dict(os.environ, PIP_NO_INDEX='1', PIP_FIND_LINKS=str(source), TMPDIR=str(root / 'tmp'))
    helper = (HELPERS / 'python-native-dependencies.py').read_text()
    for mode in ('check', 'build-dependencies', 'prepare', 'acquire', 'build', 'seal'):
        subprocess.run([sys.executable, '-c', helper, mode, json.dumps(plan)], env=environment, check=True, timeout=60)
    provenance = json.loads((source / 'operation/provenance.json').read_text())
    assert provenance['acquired'][0]['sha256'] == sha(sdist).removeprefix('sha256:')
    assert provenance['wheels']['artifacts'][0]['version'] == '1.0.0'
    directory = source / 'operation/wheels'
    target = source / 'installed'
    subprocess.run([sys.executable, '-m', 'pip', 'install', '--no-index', '--require-hashes', '--only-binary=:all:',
                    '--find-links', str(directory), '--target', str(target), '-r', str(directory / 'requirements.lock')],
                   env=environment, check=True, timeout=60)
    subprocess.run([sys.executable, '-c', 'import ato_fixture_app; assert ato_fixture_app.VALUE == 42'],
                   env=dict(environment, PYTHONPATH=str(target)), check=True, timeout=10)
    subprocess.run([sys.executable, '-c', helper, 'cleanup', json.dumps(plan)], env=environment, check=True, timeout=10)
    assert not (source / 'operation/build-env').exists()
    return dict(sdist_to_wheel=True, offline_hash_install=True, provenance=True)


def node_case(root):
    node, npm = shutil.which('node'), shutil.which('npm')
    if not node or not npm:
        return dict(status='unavailable', reason='local Node/npm test prerequisite')
    source = root / 'node'; source.mkdir()
    manifest = source / 'package.json'
    manifest.write_text(json.dumps(dict(name='fixture-app', version='1.0.0', dependencies={'fixture-native':'1.0.0'})))
    lock = source / 'package-lock.json'
    lock.write_text(json.dumps(dict(name='fixture-app', version='1.0.0', lockfileVersion=3,
        packages={'':dict(name='fixture-app', version='1.0.0', dependencies={'fixture-native':'1.0.0'}),
                  'node_modules/fixture-native':dict(version='1.0.0',hasInstallScript=True)})))
    package = source / 'node_modules/fixture-native'; package.mkdir(parents=True)
    (package / 'package.json').write_text(json.dumps(dict(name='fixture-native',version='1.0.0',
        scripts={'install':"node -e \"require('fs').writeFileSync('native-ready','ready')\""})))
    plan = dict(schema='ato.npm-native-plan/1', manifest=str(manifest),lockfile=str(lock),
                manifest_sha256=sha(manifest),lockfile_sha256=sha(lock),npm=npm,
                npm_version=subprocess.check_output([npm,'--version'],text=True).strip(),
                node_root=str(Path(node).parent.parent),node_version=subprocess.check_output([node,'-p','process.versions.node'],text=True).strip(),receipt_directory=str(source / 'receipt'),
                packages=['fixture-native'],root_lifecycle=[],toolchains=[],build_network='denied')
    helper = (HELPERS / 'node-native-dependencies.cjs').read_text()
    environment = dict(os.environ,TMPDIR=str(root / 'tmp'),npm_config_cache=str(source / 'cache'))

    def execute(mode):
        return subprocess.run([node,'-e',helper,mode,json.dumps(plan)],cwd=source,env=environment,
                              capture_output=True,text=True,timeout=30)

    assert execute('initialize').returncode == 0
    ignored = execute('audit'); assert ignored.returncode != 0 and 'npm_lifecycle_plan_required' in ignored.stderr
    assert not (package / 'native-ready').exists()
    rebuilt = execute('rebuild'); assert rebuilt.returncode == 0, rebuilt.stderr
    assert (package / 'native-ready').read_text() == 'ready'
    assert execute('audit').returncode == 0
    assert execute('initialize').returncode != 0  # source/old receipts cannot stand in for fresh completion
    return dict(ignored_scripts_refused=True, typed_rebuild=True, completion_audited=True, stale_receipt_refused=True)


def main():
    parser = argparse.ArgumentParser(); parser.add_argument('--work-root', type=Path)
    args = parser.parse_args()
    root = (args.work_root or REPO / '.tmp' / ('native-operation-smoke-' + uuid.uuid4().hex)).resolve()
    root.mkdir(parents=True,exist_ok=False); (root / 'tmp').mkdir()
    result = dict(schema='ato.native-operation-smoke/1', python=python_case(root),node=node_case(root),
                  actual_Coordinator_Runtime=False, provider_calls=0)
    (root / 'result.json').write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps(result))


if __name__ == '__main__':
    main()
