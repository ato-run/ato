# Fixed-producer, zero-known-D HTTP fixture

`oss_http.py` is the **unmodified** CPython 3.12.7 `Lib/http/server.py`, from
commit `0b05ead877f909b7efe712db758012d9dbece7ce` in
https://github.com/python/cpython . Upstream license is `LICENSE.cpython`.
`serve.py` is a preregistered, Ato-owned fixture launcher: it binds the existing
logical-port environment variable and runs that OSS server over this directory.
`bad.py` is the preregistered 404 negative control, not AI-produced repair code.

There is deliberately no capsule.toml, preset, base recipe or known D here.
The acceptance owner provides explicit K separately (GET /health → 200 with
this file's body digest). Only opaque entrypoint IDs reach the FixedProducer.
The launcher and both outcomes are fixed before the run. This tests the
compiler/durability/execution/Verifier authority path, not LLM efficacy or
unassisted coverage of arbitrary real-world applications.
