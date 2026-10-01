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


def wheel(path, name, module, source, metadata=None):
    dist = f'{name}-1.0.0.dist-info'
    with zipfile.ZipFile(path, 'w') as archive:
        archive.writestr(module + '.py', source)
        archive.writestr(dist + '/METADATA', metadata or f'Metadata-Version: 2.1\nName: {name}\nVersion: 1.0.0\n')
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
    wheel(backend, 'ato_fixture_backend', 'ato_fixture_backend', backend_code,
          'Metadata-Version: 2.1\nName: ato_fixture_backend\nVersion: 1.0.0\n'
          '\nA description may include command output:\nName: example\nVersion: 9.9.9\n')
    # Real setuptools wheels also contain vendored dist-info metadata. It must
    # not be mistaken for a second identity of the outer wheel.
    with zipfile.ZipFile(backend, 'a') as archive:
        archive.writestr('ato_fixture_backend/_vendor/packaging-1.0.dist-info/METADATA',
                         'Metadata-Version: 2.1\nName: packaging\nVersion: 1.0\n')
    sdist = source / 'ato_fixture_app-1.0.0.tar.gz'
    pyproject = b'[build-system]\nrequires=["ato-fixture-backend==1.0.0"]\nbuild-backend="ato_fixture_backend"\n'
    with tarfile.open(sdist, 'w:gz') as archive:
        info = tarfile.TarInfo('ato_fixture_app-1.0.0/pyproject.toml'); info.size = len(pyproject)
        archive.addfile(info, io.BytesIO(pyproject))
    requirements = source / 'requirements.txt'; requirements.write_text(sdist.as_uri() + '\n' + backend.as_uri() + '\n')
    plan = dict(schema='ato.python-build-plan/1', python_version=sys.version.split()[0], root=str(source / 'operation'), requirements=str(requirements),
                requirements_sha256=sha(requirements), build_dependencies=[dict(name='ato-fixture-backend', version='1.0.0')],
                toolchains=[], build_network='denied', lock_operation=(HELPERS / 'python-lock-operation.py').read_text())
    environment = dict(os.environ, PIP_NO_INDEX='1', PIP_FIND_LINKS=str(source), TMPDIR=str(root / 'tmp'))
    helper = (HELPERS / 'python-native-operation.py').read_text()
    for mode in ('check', 'build-dependencies', 'prepare', 'acquire', 'build', 'seal'):
        subprocess.run([sys.executable, '-c', helper, mode, json.dumps(plan)], env=environment, check=True, timeout=60)
    provenance = json.loads((source / 'operation/provenance.json').read_text())
    assert provenance['acquired'][0]['sha256'] == sha(sdist).removeprefix('sha256:')
    assert provenance['wheels']['artifacts'][0]['version'] == '1.0.0'
    directory = source / 'operation/wheels'
    target = source / 'installed'
    subprocess.run([sys.executable, '-m', 'pip', 'install', '--no-index', '--no-compile', '--require-hashes', '--only-binary=:all:',
                    '--find-links', str(directory), '--target', str(target), '-r', str(directory / 'requirements.lock')],
                   env=environment, check=True, timeout=60)
    assert not list(target.glob('**/*.pyc'))
    subprocess.run([sys.executable, '-c', 'import ato_fixture_app; assert ato_fixture_app.VALUE == 42'],
                   env=dict(environment, PYTHONPATH=str(target), PYTHONDONTWRITEBYTECODE='1'), check=True, timeout=10)
    completed = directory / backend.name
    original_bytes = completed.read_bytes()
    completed.write_bytes(original_bytes + b'altered-after-seal')
    refused = subprocess.run([sys.executable, '-c', helper, 'cleanup', json.dumps(plan)],
                             env=environment, capture_output=True, text=True, timeout=10)
    assert refused.returncode != 0 and 'dependency_artifact_changed' in refused.stderr
    assert (source / 'operation/acquired' / backend.name).exists()
    completed.write_bytes(original_bytes)
    subprocess.run([sys.executable, '-c', helper, 'cleanup', json.dumps(plan)], env=environment, check=True, timeout=10)
    assert not (source / 'operation/build-env').exists()
    assert not (source / 'operation/acquired' / backend.name).exists()
    assert (source / 'operation/acquired' / sdist.name).exists()
    assert sha(source / 'operation/wheels' / backend.name) == sha(backend)
    retained = next(a for a in provenance['retained_inputs'] if a['file'] == backend.name)
    assert retained['retained_path'] == 'wheels/' + backend.name
    return dict(sdist_to_wheel=True, offline_hash_install=True, provenance=True,
                description_not_an_identity=True, identical_acquired_wheel_retained_once=True,
                original_sdist_retained=True, bytecode_not_captured=True,
                changed_completed_wheel_refused_before_deduplication=True)


def wheel_metadata_refusals(root):
    helper = (HELPERS / 'python-lock-operation.py').read_text()
    cases = {
        'duplicate_name': 'Name: fixture\nname: alternate\nVersion: 1.0.0\n',
        'duplicate_version': 'Name: fixture\nVersion: 1.0.0\nVERSION: 2.0.0\n',
        'missing_identity': 'Name: fixture\n\nVersion: 1.0.0\n',
    }
    for name, metadata in cases.items():
        directory = root / ('metadata-' + name); directory.mkdir()
        wheel(directory / 'fixture-1.0.0-py3-none-any.whl', 'fixture', 'fixture',
              '', 'Metadata-Version: 2.1\n' + metadata)
        result = subprocess.run([sys.executable, '-c', helper, str(directory)],
                                capture_output=True, text=True, timeout=10)
        expected = 'dependency_wheel_metadata_' + ('invalid' if name == 'missing_identity' else 'duplicate')
        assert result.returncode != 0 and expected in result.stderr
        if name != 'missing_identity':
            evidence = json.loads(result.stderr.splitlines()[0])
            assert evidence['artifact_sha256'] == sha(directory / 'fixture-1.0.0-py3-none-any.whl').removeprefix('sha256:')
            assert evidence['count'] == 2 and 'alternate' not in result.stderr
        assert not (directory / 'artifacts.json').exists()
        assert not (directory / 'requirements.lock').exists()
    return dict(true_duplicate_headers_refused=True,
                description_cannot_supply_missing_identity=True,
                no_lock_after_invalid_identity=True)


def node_case(root):
    node, npm = shutil.which('node'), shutil.which('npm')
    if not node or not npm:
        return dict(status='unavailable', reason='local Node/npm test prerequisite')
    source = root / 'node'; source.mkdir()
    manifest = source / 'package.json'
    dependencies={'fixture-native':'1.0.0','fixture-packed':'1.0.0'}
    manifest.write_text(json.dumps(dict(name='fixture-app', version='1.0.0', dependencies=dependencies)))
    lock = source / 'package-lock.json'
    lock.write_text(json.dumps(dict(name='fixture-app', version='1.0.0', lockfileVersion=3,
        packages={'':dict(name='fixture-app', version='1.0.0', dependencies=dependencies),
                  'node_modules/fixture-native':dict(version='1.0.0',hasInstallScript=True),
                  'node_modules/fixture-packed':dict(version='1.0.0')})))
    package = source / 'node_modules/fixture-native'; package.mkdir(parents=True)
    (package / 'package.json').write_text(json.dumps(dict(name='fixture-native',version='1.0.0',
        scripts={'install':"node -e \"require('fs').writeFileSync('native-ready','ready')\""})))
    packed=source/'node_modules/fixture-packed';packed.mkdir()
    (packed/'package.json').write_text(json.dumps(dict(name='fixture-packed',version='1.0.0',
        scripts={'prepare':"node -e \"require('fs').writeFileSync('publish-only','unexpected')\""})))
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
    assert 'fixture-packed' not in ignored.stderr
    assert not (package / 'native-ready').exists()
    rebuilt = execute('rebuild'); assert rebuilt.returncode == 0, rebuilt.stderr
    assert (package / 'native-ready').read_text() == 'ready'
    assert execute('audit').returncode == 0
    assert not (packed/'publish-only').exists()
    assert execute('initialize').returncode != 0  # source/old receipts cannot stand in for fresh completion
    return dict(ignored_scripts_refused=True, typed_rebuild=True, completion_audited=True,
                stale_receipt_refused=True, packed_prepare_not_an_install_prerequisite=True)


def runtime_scripts_case(root):
    node, npm = shutil.which('node'), shutil.which('npm')
    if not node or not npm:
        return dict(status='unavailable', reason='local Node/npm test prerequisite')
    source = root / 'runtime-scripts'; source.mkdir(); (source / 'state').mkdir()
    manifest = source / 'package.json'
    manifest.write_text(json.dumps(dict(name='source-runtime-fixture', version='1.0.0', scripts={
        'initialize': 'node -e "require(\'fs\').writeFileSync(\'state/initialized\', process.env.FIXTURE_PRIVATE_INPUT ? \'bound\' : \'missing\')"',
        'start': 'node -e "const f=require(\'fs\');if(f.readFileSync(\'state/initialized\',\'utf8\')!==\'bound\')process.exit(1);f.writeFileSync(\'state/launched\',\'ready\')"',
        'fail-setup': 'node -e "process.exit(7)"'})))
    plan = dict(manifest=str(manifest),manifest_sha256=sha(manifest),npm=npm,
                setup_scripts=['initialize'],launch_script='start',argv=[])
    helper = (HELPERS / 'node-runtime-scripts.cjs').read_text()
    environment = dict(os.environ,TMPDIR=str(root / 'tmp'),FIXTURE_PRIVATE_INPUT='fixture-only-private-value',
                       npm_config_cache=str(source / 'cache'))

    def execute():
        result = subprocess.run([node,'-e',helper,json.dumps(plan)],cwd=source,env=environment,
                                capture_output=True,text=True,timeout=30)
        assert environment['FIXTURE_PRIVATE_INPUT'] not in result.stdout + result.stderr
        return result

    success = execute(); assert success.returncode == 0, success.stderr
    assert (source / 'state/launched').read_text() == 'ready'
    (source / 'state/launched').unlink(); (source / 'state/initialized').unlink()
    plan['setup_scripts'] = ['fail-setup']
    failure = execute(); assert failure.returncode != 0 and 'source_runtime_setup_failed' in failure.stderr
    assert not (source / 'state/launched').exists()
    plan['setup_scripts'] = ['initialize']
    manifest.write_text(manifest.read_text() + '\n')
    altered = execute(); assert altered.returncode != 0 and 'source_runtime_manifest_changed' in altered.stderr
    assert not (source / 'state/initialized').exists()
    return dict(setup_before_launch=True,same_state_and_private_environment=True,
                setup_failure_prevents_launch=True,manifest_checked_before_effects=True,
                private_value_not_reported=True)


def main():
    parser = argparse.ArgumentParser(); parser.add_argument('--work-root', type=Path)
    args = parser.parse_args()
    root = (args.work_root or REPO / '.tmp' / ('native-operation-smoke-' + uuid.uuid4().hex)).resolve()
    root.mkdir(parents=True,exist_ok=False); (root / 'tmp').mkdir()
    result = dict(schema='ato.native-operation-smoke/1', python=python_case(root),
                  wheel_metadata=wheel_metadata_refusals(root), node=node_case(root),
                  runtime_scripts=runtime_scripts_case(root),
                  actual_Coordinator_Runtime=False, provider_calls=0)
    (root / 'result.json').write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps(result))


if __name__ == '__main__':
    main()
