"""E2 prospective holdout fixtures. Ground truth stays outside provider input.

All twelve cases are new committed files under
apps/formation-worker/fixtures/runtime-network/e2-holdout; no E1 candidate
bytes, wrappers, paddings, encoding cookies or opaque IDs are reused. Fixture
bytes never branch on the arm or on any expected selector outcome; the only
permutation difference is which file each opaque ID maps to. Intended labels
live only in the preregistration.
"""
from pathlib import Path
import hashlib
import shutil

# Sorted case IDs are also the preregistered execution order.
CASES = [
    'C01', 'C02', 'C03', 'C04',
    'D01', 'D02', 'D03',
    'L01', 'L02',
    'P01', 'P02', 'P03',
]
IDS = ['k4', 'v9']

FAMILY = {
    'C': 'control',      # context/1-identifiable, both-valid, both-invalid, indistinguishable
    'D': 'delegation',   # runpy wrapper positive, HTTP-looking negative
    'L': 'latin1',       # explicit supported Latin-1 positive, plausible negative
    'P': 'prefix',       # >64KiB bounded-prefix positive evidence, plausible decoy
}

# Intended label per case: candidate file indices satisfying the frozen K.
# Registered before any execution; never provider input.
CORRECT = {
    'C01': [0], 'C02': [0, 1], 'C03': [], 'C04': [0],
    'D01': [0], 'D02': [0], 'D03': [0],
    'L01': [0], 'L02': [0],
    'P01': [0], 'P02': [0], 'P03': [0],
}


def materialize(notes: Path, holdout: Path, destination: Path, case: str, permutation: int):
    """Write the frozen source tree; return (entrypoint map, file digests).

    The same bytes are produced for every arm of a case/permutation. Swapping
    the permutation exchanges only which file each opaque ID resolves to.
    """
    assert case in CASES and permutation in (0, 1)
    shutil.copytree(notes, destination)
    for source in sorted((holdout / case).iterdir()):
        shutil.copyfile(source, destination / source.name)
    shutil.copyfile(holdout / 'bad.py', destination / 'bad.py')
    shutil.copyfile(holdout / 'private-source.txt', destination / 'private-source.txt')
    entries = {IDS[i]: f'candidate_{i ^ permutation}.py' for i in range(2)}
    hashes = {str(p.relative_to(destination)): hashlib.sha256(p.read_bytes()).hexdigest()
              for p in sorted(destination.rglob('*')) if p.is_file()}
    return entries, hashes
