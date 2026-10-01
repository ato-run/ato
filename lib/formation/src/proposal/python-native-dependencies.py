"""Registered, generic sdist-to-wheel operations inside the build sandbox.

The Runtime owns process-group, deadline, filesystem, network and resource
limits. This operation never grants networking or runs outside that boundary.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import shutil
import sys


def digest(path):
    if path.is_symlink() or not path.is_file():
        raise ValueError('dependency_artifact_not_regular')
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def write(path, value):
    with path.open('x') as stream:
        json.dump(value, stream, sort_keys=True, separators=(',', ':'))


def artifacts(directory):
    paths = sorted(directory.iterdir())
    if not paths or len(paths) > 512:
        raise ValueError('dependency_artifact_count_limit')
    result, total = [], 0
    for path in paths:
        size = path.stat().st_size
        total += size
        if size > 512 * 1024**2 or total > 2 * 1024**3:
            raise ValueError('dependency_artifact_byte_limit')
        result.append(dict(file=path.name, sha256=digest(path), bytes=size))
    return result


def native_tools(selections):
    evidence, paths, environment = [], [], {}
    for tool in selections:
        name, version = tool['name'], tool['version']
        directory = Path(f'/opt/ato/toolchains/{name}/{version}/bin')
        program = directory / ('python3' if name == 'python' else name)
        args = ['-dumpfullversion', '-dumpversion'] if name == 'gcc' else ['--version']
        actual = subprocess.check_output([str(program), *args], text=True, timeout=10).splitlines()[0]
        reported = actual if name in ('gcc', 'pkg-config') else actual.split()[-1]
        normalized = '.'.join((reported.split('.') + ['0', '0'])[:3])
        if normalized != version:
            raise ValueError('native_toolchain_version_mismatch')
        resolved = program.resolve(strict=True)
        evidence.append(dict(name=name, version=version, executable_sha256=digest(resolved)))
        paths.append(str(directory))
        if name == 'python':
            environment.update(PYTHON=str(program), npm_config_python=str(program))
        if name == 'gcc':
            cxx = directory / 'g++'
            evidence[-1]['cxx_sha256'] = digest(cxx.resolve(strict=True))
            environment.update(CC=str(program), CXX=str(cxx))
        if name == 'make':
            environment['MAKE'] = str(program)
        if name == 'pkg-config':
            environment['PKG_CONFIG'] = str(program)
    environment['PATH'] = ':'.join(paths + [os.environ.get('PATH', '/usr/bin:/bin')])
    return evidence, environment


def execute(mode, plan):
    if plan['schema'] != 'ato.python-build-plan/1':
        raise ValueError('python_build_plan_invalid')
    if sys.version.split()[0] != plan['python_version']:
        raise ValueError('native_toolchain_version_mismatch')
    root = Path(plan['root'])
    requirements = Path(plan['requirements'])
    if 'sha256:' + digest(requirements) != plan['requirements_sha256']:
        raise ValueError('proposal_source_digest_mismatch')
    build_deps, acquired, wheels = (root / name for name in ('build-dependencies', 'acquired', 'wheels'))
    environment = dict(os.environ, PIP_NO_INPUT='1', PIP_DISABLE_PIP_VERSION_CHECK='1', PIP_CACHE_DIR=str(root / 'cache'),
                       CC='/ato/unbound/gcc', CXX='/ato/unbound/g++', MAKE='/ato/unbound/make',
                       PKG_CONFIG='/ato/unbound/pkg-config', RUSTC='/ato/unbound/rustc', CARGO='/ato/unbound/cargo')
    environment['PYTHONPATH'] = ''
    python = str(root / 'build-env/bin/python3')

    def run(argv):
        subprocess.run(argv, check=True, env=environment)

    if mode == 'check':
        root.mkdir(parents=True, exist_ok=False)
        for directory in (build_deps, acquired, wheels):
            directory.mkdir()
        tool_evidence, _ = native_tools(plan['toolchains'])
        tool_evidence.insert(0, dict(name='build_python', version=plan['python_version'],
                                     executable_sha256=digest(Path(sys.executable).resolve(strict=True))))
        write(root / 'toolchains.json', tool_evidence)
    elif mode == 'build-dependencies':
        run([sys.executable, '-m', 'pip', 'download', '--no-input', '--only-binary=:all:', '--dest', str(build_deps),
             *[f"{d['name']}=={d['version']}" for d in plan['build_dependencies']]])
        run([sys.executable, '-c', plan['lock_operation'], str(build_deps)])
    elif mode == 'prepare':
        # Bundled ensurepip is part of the pinned Python distribution. Installing
        # build backends then uses only the acquired, hash-locked wheel artifacts.
        run([sys.executable, '-m', 'venv', str(root / 'build-env')])
        run([python, '-m', 'pip', 'install', '--no-input', '--no-index', '--only-binary=:all:', '--require-hashes',
             '--find-links', str(build_deps), '-r', str(build_deps / 'requirements.lock')])
    elif mode == 'acquire':
        # Metadata hooks for an sdist also run in the contained dependency phase
        # with only the explicitly installed build backends and allowed network.
        _, native_environment = native_tools(plan['toolchains'])
        environment.update(native_environment)
        run([python, '-m', 'pip', 'download', '--no-input', '--no-build-isolation', '--dest', str(acquired), '-r', str(requirements)])
        write(root / 'acquired.json', artifacts(acquired))
    elif mode == 'build':
        inputs = json.loads((root / 'acquired.json').read_text())
        _, native_environment = native_tools(plan['toolchains'])
        environment.update(native_environment)
        if artifacts(acquired) != inputs:
            raise ValueError('dependency_artifact_changed')
        run([python, '-m', 'pip', 'wheel', '--no-input', '--no-index', '--no-deps', '--no-build-isolation',
             '--wheel-dir', str(wheels), *[str(acquired / artifact['file']) for artifact in inputs]])
        if artifacts(acquired) != inputs:
            raise ValueError('dependency_artifact_changed')
    elif mode == 'seal':
        run([sys.executable, '-c', plan['lock_operation'], str(wheels)])
        # Set-level lineage includes all resolution inputs, build dependencies
        # and toolchain bytes; output wheels additionally name version + hash.
        provenance = dict(schema='ato.python-wheel-build/1', requirements_sha256=plan['requirements_sha256'],
                          acquired=json.loads((root / 'acquired.json').read_text()),
                          build_dependencies=json.loads((build_deps / 'artifacts.json').read_text()),
                          toolchains=json.loads((root / 'toolchains.json').read_text()),
                          build_network=plan['build_network'], metadata_network='scoped-dependencies',
                          build_isolation='Runtime sandbox and dedicated build-env',
                          wheels=json.loads((wheels / 'artifacts.json').read_text()))
        if artifacts(acquired) != provenance['acquired']:
            raise ValueError('dependency_artifact_changed')
        # An acquired wheel is copied unchanged by pip wheel. Preserve its
        # identity once, alongside the original sdists and completed wheels.
        outputs = {a['file']: a for a in provenance['wheels']['artifacts']}
        retained_inputs = []
        for artifact in provenance['acquired']:
            output = outputs.get(artifact['file'])
            unchanged = (artifact['file'].endswith('.whl') and output
                         and output['sha256'] == artifact['sha256']
                         and output['bytes'] == artifact['bytes'])
            retained_inputs.append(dict(artifact, retained_path=(
                'wheels/' if unchanged else 'acquired/') + artifact['file']))
        provenance['retained_inputs'] = retained_inputs
        write(root / 'provenance.json', provenance)
        print(json.dumps(dict(schema=provenance['schema'], wheel_count=len(provenance['wheels']['artifacts']))))
    elif mode == 'cleanup':
        provenance = json.loads((root / 'provenance.json').read_text())
        if artifacts(acquired) != provenance['acquired']:
            raise ValueError('dependency_artifact_changed')
        for artifact in provenance['retained_inputs']:
            name = artifact['file']
            if Path(name).name != name:
                raise ValueError('dependency_artifact_path_invalid')
            if artifact['retained_path'] == 'wheels/' + name:
                original, completed = acquired / name, wheels / name
                if (not name.endswith('.whl')
                        or digest(original) != artifact['sha256']
                        or digest(completed) != artifact['sha256']
                        or original.stat().st_size != completed.stat().st_size):
                    raise ValueError('dependency_artifact_changed')
                original.unlink()
        # Generated build inputs and pinned wheels/provenance stay. A venv's
        # interpreter symlinks and transient pip cache are not retained output.
        for directory in (root / 'build-env', root / 'cache'):
            if directory.exists():
                shutil.rmtree(directory)
    else:
        raise ValueError('python_build_operation_invalid')


try:
    execute(sys.argv[1], json.loads(sys.argv[2]))
except Exception as error:
    # Runtime log redaction owns any underlying subprocess output. This marker
    # contains no exception text or secret-bearing URLs/environment.
    code = str(error) if isinstance(error, ValueError) else 'python_native_operation_failed'
    print('ATO_FORMATION_FAILURE ' + json.dumps(dict(code=code, message='Python dependency operation failed; inspect bounded build evidence')), file=sys.stderr)
    raise SystemExit(1)
