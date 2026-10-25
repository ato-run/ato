#!/usr/bin/env python3
"""Keep registered Python code bounded without changing its sandbox or phases.

The readable source files remain authoritative. Checked-in loaders contain only
those exact bytes, so Rust and WASM lower the same code without a new dependency.
"""
import argparse
import ast
import base64
from pathlib import Path
import zlib

ROOT = Path(__file__).resolve().parents[3] / 'lib/formation/src/proposal'


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    for source, target in [('python-native-dependencies.py', 'python-native-operation.py'),
                           ('python-dependency-lock.py', 'python-lock-operation.py')]:
        raw = (ROOT / source).read_bytes()
        output = ROOT / target
        if args.check:
            tree = ast.parse(output.read_text())
            calls = [n for n in ast.walk(tree) if isinstance(n, ast.Call)
                     and isinstance(n.func, ast.Attribute) and n.func.attr == 'b85decode']
            assert len(calls) == 1
            encoded = ast.literal_eval(calls[0].args[0])
            assert zlib.decompress(base64.b85decode(encoded)) == raw, target + ' is stale'
        else:
            encoded = base64.b85encode(zlib.compress(raw, 9)).decode('ascii')
            output.write_text('# Generated from ' + source + '; regenerate with '
                              'scripts/acceptance/coverage/generate-python-operations.py\n'
                              'import base64, zlib\nexec(zlib.decompress(base64.b85decode('
                              + repr(encoded) + ')))\n')
        print(target, output.stat().st_size)


if __name__ == '__main__':
    main()
