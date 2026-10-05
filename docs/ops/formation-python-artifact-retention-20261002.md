# Python artifact retention — 2026-10-02

At frozen Ato bbd368771732b14ac995a466f18ac54c67c2f0de / API
a22b62df51f1a0d02746f56f1fd7ec7c670d81eb, changedetection.io trial 10
completed its sdist-capable dependency operation, sealed and launched in the
normal contained Runtime. Fresh receipt fully_satisfied=true used the unchanged
K ba864ae817c507b0b277c2d657390ba25fb7f78ecc9566150fd0a7040be36fb6,
D 88c6ebd175120b025ba53993ce4a22ba71906c078facde0cb33a140251c33445,
attempt 01M3WT0R9R8ZAK4AWV11PWK5Y3, root HTTP 200 and original source identity.
Publication then failed: artifact 831,707,648 bytes versus the existing
536,870,912-byte ticket cap. Cleanup succeeded. The Search ended budget_exhausted
with one attempt / one round / two Codex session exchanges and no submission.
Result SHA-256: 390db41ef66928a5289a35b6f5759afbe8cf8bb4cf2a6ed12181f2ba0844f352.
This is a typed-K PASS with failed publication, not a usable submitted D or
completed functional acceptance. Zero provider API calls; actual Codex calls,
tokens and fee remain unknown. Preserve the original trial and byte budgets.

The generic operation currently retains acquired prebuilt wheels and unchanged
copies in the completed wheel directory, and offline pip install generates
bytecode into the reusable artifact. Avoid bytecode compilation during that
install. During cleanup, discard an acquired wheel only after its digest and
size match the retained completed wheel. Keep original sdists, completed wheels,
version/hash metadata and build/toolchain/network evidence. Add retained_inputs
to provenance so the acquired identity points to its remaining exact bytes.
No storage cap, source requirements, K, or permission is expanded.

Run the builtin Python metadata, hash and installer operations with isolated
mode (-I), including their child pip/venv commands. They execute in the source
working directory but must use the pinned interpreter's modules, not same-named
source modules or ambient PYTHONPATH. Explicit PEP 517 source backends remain
part of the contained build operation; application launch keeps its declared
source import behavior.

Validation: real offline pip/PEP 517 integration confirms original sdist and
completed wheels remain, identical acquired wheel is stored once, no bytecode
is captured, and mutation of a completed wheel prevents deduplication before
the original input is deleted. Hash install, metadata refusal, Node rebuild
and private Runtime setup fixtures pass. All 119 Formation library tests and
generated-source/format checks pass. The real pip fixture runs from a source
directory containing failing hashlib, email, pip and base64 modules and confirms
none can shadow builtin tooling. Final-code actual application publication
and matching rebuilt authority remain required. No deployment, remote migration,
ordinary Run or 100-case measurement.
