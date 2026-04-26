# Show HN: ato – run any project instantly with one command, no setup

**Title:** Show HN: ato – run any project instantly with one command, no setup

---

**Post body:**

Hi HN,

I built `ato` because I kept hitting the same friction: someone shares a Python project or a Node app, and before you can try it you're reading a README, setting up a virtualenv, installing node_modules, and debugging why their setup.sh doesn't work on your machine.

`ato` removes that layer:

```
curl -fsSL https://ato.run/install.sh | sh

ato run github.com/owner/repo
ato run hello.py
ato run https://ato.run/s/demo@r1
```

It reads the project directly — `pyproject.toml`, `package.json`, `deno.json`, `Cargo.toml`, a bare script — and runs it in a sandboxed environment. No Dockerfile, no config to write.

**Share a project:**

```
ato encap   → https://ato.run/s/my-project@r1
```

Anyone can then `ato run <url>` to rebuild and run it. Secrets are never uploaded; `encap` records *contracts* (which env vars are needed) but not their values.

**Runtimes supported today:** Python (uv-backed, single-file PEP 723), Node/TypeScript/Deno, Rust, Go, Wasm/Wasmtime, OCI containers, shell scripts.

**Security model:** Python and native source runtimes route through [Nacelle](https://github.com/ato-run/nacelle), a sandboxed runtime. `network.enabled = false` in `capsule.toml` blocks all outbound traffic at the OS level (`sandbox-exec` on macOS, `bwrap` on Linux). No network by default for unknown code.

**Why not just use Docker?** Docker is great for reproducible deployments. `ato` is for the moment before that: running, trying, and sharing code without the container overhead and without learning a new DSL.

**Known limitations (honest):** `egress_allow` hostname filtering is advisory on source runtimes in v0.5 (deny-all is enforced), and `required_env` warns but doesn't abort yet. Full list: https://github.com/ato-run/ato-cli/blob/main/docs/known-limitations.md

**ato-samples:** https://github.com/ato-run/ato-samples — 12+ runnable examples from quickstart to BYOK AI chat.

Repo: https://github.com/ato-run/ato-cli  
Install: `curl -fsSL https://ato.run/install.sh | sh`

Happy to answer questions about the sandbox design, the share URL format, or the Capsule Protocol spec.

---

## Anticipated HN questions & answers

**Q: How is this different from `uvx` / `pipx` / `npx`?**  
A: Those handle single packages within one ecosystem. `ato` is polyglot and handles full multi-file projects, not just CLI tool installation. It also provides network/filesystem sandboxing and shareable workspace snapshots.

**Q: How is this different from Nix?**  
A: Nix reproduces environments declaratively; you have to write Nix expressions. `ato` infers the environment from existing project files and runs it with zero config. Different ergonomic target.

**Q: What is the Capsule Protocol?**  
A: A manifest spec (`capsule.toml`) that declares runtime, network policy, required env, and build lifecycle. The goal is for multiple runtimes to be able to implement it so capsules aren't locked to `ato`. The conformance suite is a skeleton today — building it out is the next major priority.

**Q: macOS only?**  
A: Linux, macOS, and Windows (x64 and arm64). Windows sandbox enforcement uses different mechanisms.

**Q: What about GPU workloads?**  
A: GPU capsules work — `ato run` passes through CUDA/Metal/ROCm. But raw GPU device handle passthrough is not sandboxed yet (L: no-raw-gpu-handle in samples/03-limitations/).

**Q: Open source?**  
A: Yes, Apache 2.0. https://github.com/ato-run/ato-cli
