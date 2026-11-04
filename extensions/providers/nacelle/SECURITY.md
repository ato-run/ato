# Security Policy — Nacelle provider

Status: Current. This page is the provider-specific threat model and the
reporting entry point for Nacelle; it does not define a second project policy.

Nacelle (`extensions/providers/nacelle`) is the OS-native sandbox and
source-execution provider Ato uses to realize a computation on a host. The
reporting contact, scope, disclosure terms and incident response for the
project are owned by the official [Ato Security
Policy](https://ato.run/legal/security/). Read that page for current terms.

## Reporting a vulnerability

**Do not open a public GitHub issue or pull request for a vulnerability.**

1. Report through the official [Ato Security Policy](https://ato.run/legal/security/)
   to `kohsuke@ato.run`. The same contact and its expiry are published in
   [`security.txt`](https://ato.run/.well-known/security.txt). Reports in
   English or Japanese are welcome.
2. Include what is needed to reproduce the finding:
   - the affected component and exact version (release tag or commit);
   - platform and OS version;
   - the `capsule.toml` isolation settings involved (`[isolation]`,
     `[isolation.network]`, the `[isolation.filesystem]` paths, and development
     mode);
   - reproduction steps and the impact you believe it has;
   - for a sandbox finding, which boundary you crossed: filesystem, network,
     IPC/Unix socket, process, or macOS Keychain mach services.
3. Do not disclose publicly before a mitigation is available, and use only
   accounts and data you own or have permission to test. Those are the terms of
   the official policy.

GitHub private vulnerability reporting is not an alternate entry point: the
feature is currently disabled for this repository, so a report sent only there
would not reach anyone. Use the official contact above.

## Scope

In scope for this page, as part of the Ato CLI and runtime published by the
project:

- sandbox policy construction and application on macOS (Seatbelt) and Linux
  (Bubblewrap with Landlock);
- sensitive-path protection and filtering of user-declared allow-lists;
- the command-execution boundary and its portability checks;
- network isolation, including what it does and does not enforce.

The official policy defines the service scope — `ato.run`, `api.ato.run`, app
execution origins under `ato.run`, and the Ato CLI and runtime published by the
project — and excludes software published by other users, denial-of-service
testing, social engineering and physical attacks. A Nacelle finding inside that
scope is reported the same way.

## Supported versions

Ato publishes CLI and runtime artifacts in
[GitHub Releases](https://github.com/ato-run/ato/releases); the provider crate
version is declared in its `Cargo.toml`. This page does not maintain a
version-by-version support matrix and promises no fixed support window. Report
the version you actually ran.

## Severity

Classified by what a workload gains beyond what its declared policy allows:

- **Critical** — escapes the sandbox: host code execution outside the declared
  workspace, or read access to a default-denied sensitive path.
- **High** — privilege escalation on the host, significant data exposure, or a
  bypass of network isolation when the workload is not granted network access.
- **Medium** — limited information disclosure, or a policy weakening that needs
  an additional precondition to exploit.
- **Low** — hardening recommendations and non-security defects.

## Response and disclosure

The official policy states the response process: reports are acknowledged,
triaged, contained where needed, and reporters are kept informed; affected
listings or accounts may be disabled while remediation is in progress, and
affected users and authorities are notified where required by law. It does not
publish fixed acknowledgement or patch deadlines, so this page promises none —
use the official policy for current terms. Reporters are asked to hold public
disclosure until a mitigation is available; a public advisory (and CVE where
applicable) is published with the fix.

---

## Threat model

Nacelle runs commands on the host. The declared isolation policy, lowered to
OS-native mechanisms, is the security boundary — there is no binary allowlist
standing in front of it.

### Command execution: no binary allowlist

Nacelle does **not** maintain a binary allowlist for capsule commands. Any
command declared in `capsule.toml` (`execution.entrypoint` /
`execution.command`) may execute.

**Rationale:** allowing `npm` or `python` already means arbitrary code can
run — `npm run dev` executes whatever `package.json` contains, and
`python script.py` can do anything Python can do. A binary allowlist creates a
false sense of security and breaks extensibility. The real boundary is the OS
sandbox.

Only basic portability checks are enforced:

- no absolute paths (`/usr/bin/python` → use `python`);
- no directory traversal (`../../../bin/sh`).

### The boundary is the OS sandbox

| Platform | Primary            | Supplementary |
| -------- | ------------------ | ------------- |
| macOS    | Seatbelt (SBPL)    | —             |
| Linux    | Bubblewrap (bwrap) | Landlock LSM  |

#### macOS: Seatbelt with dynamic SBPL

Profiles are generated per call from the target's isolation policy as SBPL and
applied in one of two ways: the source launcher writes the profile to a per-run
file under the runtime state directory and starts the workload with
`sandbox-exec -f <profile>`; the supervisor path applies a policy in-process
through `sandbox_init(flags = 0)`, which parses raw SBPL source in the kernel.
`sandbox-exec` is only a wrapper for a generated profile, never a predefined
profile name.

The two profile generators are not identical, and the difference matters when
reading a profile:

| Path | Default | What constrains the workload |
| --- | --- | --- |
| Source launcher (`generate_seatbelt_profile`) | `(allow default)` | explicit `(deny file-read* file-write* (subpath ...))` rules for every path in `sensitive_paths()`, plus `(deny network*)` when network is not granted |
| Supervisor policy (`generate_sbpl_profile`) | `(deny default)` | explicit allows for declared read-only and read-write paths, process and IPC primitives, and network only when granted; Mach services that gate the Keychain (`com.apple.secd`, `com.apple.SecurityServer`, `com.apple.security.agent`, `com.apple.authorizationd`) are explicitly denied |

In both, the sensitive-path deny list is what protects user secrets: in the
supervisor profile it is one rule inside a default-deny policy, and in the
launcher profile it carries the boundary. IPC sockets are emitted both as file
operations (stat/unlink on the inode) and as `network*`
`(remote/local unix-socket (subpath ...))` rules for connect/bind on the AF_UNIX
socket.

#### Linux: Bubblewrap plus Landlock

1. **Bubblewrap** provides PID/mount/net namespace isolation: only
   explicitly bind-mounted paths are visible, sensitive paths are hidden with
   `--tmpfs` overlays, all capabilities are dropped (`--cap-drop ALL`), and
   `--unshare-all` without `--share-net` leaves the child in a fresh network
   namespace where outbound `connect()` fails. Production mode additionally
   clears the environment (`--clearenv`) before re-adding a small allow-list.
2. **Landlock** (kernel 5.13+) adds filesystem access control. It is applied to
   the workload from inside the sandbox by the `nacelle sandbox-exec` shim,
   after namespace setup, using an allow-list built from `capsule.toml` after
   sensitive paths are filtered.

### Sensitive paths: default deny list

The following user directories are blocked by default across all platforms.
They are defined in `src/system/sandbox/mod.rs::sensitive_paths()`.

| Path                                                    | Reason              |
| ------------------------------------------------------- | ------------------- |
| `~/.ssh`                                                | SSH keys            |
| `~/.gnupg`                                              | GPG keys            |
| `~/.aws`                                                | AWS credentials     |
| `~/.kube`                                               | Kubernetes config   |
| `~/.config/gcloud`                                      | GCP credentials     |
| `~/.azure`                                              | Azure credentials   |
| `~/.docker`                                             | Docker credentials  |
| `~/.npmrc`                                              | npm tokens          |
| `~/.pypirc`                                             | PyPI tokens         |
| `~/.bash_history` / `~/.zsh_history`                    | May contain secrets |
| `~/Library/Keychains` _(macOS)_                         | System keychain     |
| `~/Library/Cookies` _(macOS)_                           | Browser cookies     |
| `~/Library/Application Support/Google/Chrome` _(macOS)_ | Browser profile     |
| `~/Library/Application Support/Firefox` _(macOS)_       | Browser profile     |

**When a user declares a parent path:** if an `[isolation.filesystem]` entry
names a directory that also contains a default-denied path (a home directory,
for example), those sensitive sub-directories are removed from the allow-list;
on macOS the Seatbelt `(deny ...)` rules override the parent `(allow ...)`, and
on Linux `filter_sensitive_paths()` removes them before the Landlock rules and
Bubblewrap overlays are built. A `WARN` log is emitted when paths are filtered.

### Network egress control

| Input                                | Scope        | Linux                                                | macOS                                  |
| ------------------------------------ | ------------ | ---------------------------------------------------- | -------------------------------------- |
| `isolation.network.enabled = false`  | All traffic  | No host network namespace                            | `(deny network*)`                      |
| `egress_allow` (domains)             | L7 domains   | `WARN`; delegated to the tsnet sidecar               | `WARN` plus profile comment; delegated |
| `egress_id_allow` (CIDRs)            | L3 addresses | Launch refused without a tsnet sidecar (fail closed) | `WARN` plus profile comment only       |

Domain- and CIDR-level filtering cannot be enforced by the OS sandbox: Seatbelt
supports only IP-based rules and Landlock has no network filtering at all. The
two platforms are not equivalent for `egress_id_allow` — the Linux launcher
refuses to start when the rule cannot be enforced, while the macOS path only
warns, so a CIDR rule there is advisory. Treat any `egress_allow` /
`egress_id_allow` restriction as requiring the sidecar to actually be running,
not as an OS-enforced boundary.

### Known limits

- **Development mode is not a boundary.** With `development_mode`
  (`dev_mode`), the macOS launcher runs the workload without Seatbelt and the
  in-process sandbox application reports `not_enforced`; on Linux `dev_mode`
  shares the host network namespace and skips the production environment
  hardening. Use it for debugging only.
- **Nacelle is not a container runtime or a VM.** It applies OS-native
  mechanisms on the host and does not provide the same boundary as Docker or a
  virtual machine; use Docker or a VM when you need stronger isolation (see the
  [provider README](README.md#security-boundary)).
- **No allowlist of executables.** Correctness of a declared command is the
  author's responsibility; the sandbox, not the command name, constrains it.
- **Denied paths are a fixed list.** A path outside the list that the user
  explicitly grants is granted by design.

## Security-relevant development checks

Nacelle is excluded from the root Cargo workspace and builds from its own
manifest. Before a pull request that touches the sandbox, launcher or
sensitive-path code, run from `extensions/providers/nacelle`:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Review sandbox-profile generation and allow-list filtering changes with their
unit tests, and state in the pull request which boundary the change touches and
what was not verified on a real host. If you change dependencies, run
`cargo audit` locally when it is installed; this repository has no scheduled
dependency-audit job, so do not assume a background scan covers a change.

## Acknowledgments

Thank you to the security researchers who report vulnerabilities responsibly and
help improve the provider's security posture.

## Additional resources

- [Official Ato Security Policy](https://ato.run/legal/security/) and
  [`security.txt`](https://ato.run/.well-known/security.txt)
- [Ato repository README](../../../README.md) and
  [contributing guide](../../../CONTRIBUTING.md)
- [OWASP Top 10](https://owasp.org/www-project-top-ten/)
- [Rust Security Guidelines](https://anssi-fr.github.io/rust-guide/)
- [Landlock](https://landlock.io/) and [Bubblewrap](https://github.com/containers/bubblewrap)

---

Last updated: 2026-11-04. Maintained with the Ato repository.
