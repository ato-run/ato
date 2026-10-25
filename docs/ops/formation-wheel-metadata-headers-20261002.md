# Wheel metadata header boundary — 2026-10-02

The generic dependency seal operation scanned every METADATA line for Name and
Version. A valid description body could therefore be mistaken for a duplicate
identity. Core metadata uses email headers and permits the description after a
blank separator ([PyPA specification](https://packaging.python.org/en/latest/specifications/core-metadata/)).
Use the stdlib compat32 header parser; retain one root dist-info, UTF-8 and byte
limits, reject malformed/missing identities and duplicate case-insensitive
headers, and keep exact wheel hashing and offline hash installation. Refused
duplicate headers report only the wheel digest, field and count, never contents.
Regenerate the registered loader from the readable source.

Validation: the previous aafa parser rejects the valid description fixture with
dependency_wheel_metadata_duplicate. The revised real pip/PEP 517 fixture passes
sdist build, backend locking with nested vendored metadata, provenance and
offline hash installation. True Name/Version duplicates and an identity present
only in the body remain refused before producing a lock. Existing Node rebuild
and private Runtime setup fixtures pass. All 119 Formation library tests pass;
generated loader source check and Python compilation pass.

These are operation fixtures, not real Coordinator/Runtime application success.
The e02 changedetection.io trial 9 reported python_native_operation_failed with
dependency_wheel_metadata_duplicate, then ended at the original round deadline.
Its failed artifact was cleaned up and the bounded evidence did not identify
the wheel, so this fix is not yet proved to explain that specific failure.
Keep that Search, its 384 MiB attempt reservation and deadlines unchanged.
Result SHA-256: bb3241e7ea25130d33a081ece0451ba775b16e0fbfa0c80f20e19a1c7a8e9cf6.
A fresh frozen-code application trial and matching rebuilt API authority are
required. No deployment, remote migration, ordinary Run or 100-case run.
