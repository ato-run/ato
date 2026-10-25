"""E1 fixed fixtures. Ground truth stays outside the frozen provider source."""
from pathlib import Path
import hashlib
import shutil

CASES = [f'E{i:02}' for i in range(1, 11)]
IDS = ['q7', 'm2']

def materialize(notes: Path, destination: Path, case: str, permutation: int):
    assert case in CASES and permutation in (0, 1)
    shutil.copytree(notes, destination)
    good = (notes / 'app.py').read_bytes()
    bad = good.replace(b'self.send_response(200)', b'self.send_response(404)')
    cli = b"print('finite invocation')\n"
    wrapper = b'import runpy\nrunpy.run_path("app.py", run_name="__main__")\n'
    decoy = b'if False:\n    import flask, fastapi, uvicorn\n' + bad
    alternatives = {
        'E01': (good, cli),
        'E02': (good, good + b'\n'),
        'E03': (good, decoy),
        'E04': (wrapper, bad),
        'E05': (good, bad),
        'E06': (good + b'\n#' + b'x' * 66000 + b'\n', bad),
        'E07': (b'# coding: latin-1\n# \xff\n' + good, bad),
        'E08': (good, b'from http.server import SimpleHTTPRequestHandler, HTTPServer\n'
                        b'HTTPServer(("127.0.0.1",8000),SimpleHTTPRequestHandler).serve_forever()\n'),
        'E09': (good, bad),
        'E10': (cli, b"print('no service')\n"),
    }[case]
    for i, data in enumerate(alternatives):
        (destination / f'candidate_{i}.py').write_bytes(data)
    # Identical across arms/permutations, never include run salts or oracle labels.
    (destination / 'bad.py').write_bytes(b"raise RuntimeError('fixed parent failure')\n" if case == 'E08' else bad)
    (destination / 'bad_b.py').write_bytes(bad + b'\n')
    (destination / 'private-source.txt').write_text('EFFICACY_PRIVATE_CANARY\n')
    entries = {IDS[i]: f'candidate_{i ^ permutation}.py' for i in range(2)}
    hashes = {str(p.relative_to(destination)): hashlib.sha256(p.read_bytes()).hexdigest()
              for p in sorted(destination.rglob('*')) if p.is_file()}
    return entries, hashes
