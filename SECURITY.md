# Security Policy

## Reporting Security Vulnerabilities

The Nacelle project takes security seriously. If you discover a security vulnerability, **please do not open a public GitHub issue**. Instead, follow this responsible disclosure process:

### How to Report

1. **Email Security Team:** Send a detailed report to `security@nacelle.dev` (if available) or create a **private security advisory** on GitHub:
   - Navigate to: **Settings → Security → Private Vulnerability Reporting**
   - Report the vulnerability using GitHub's Security Advisory form

2. **Include in Report:**
   - Clear description of the vulnerability
   - Steps to reproduce (if applicable)
   - Affected version(s)
   - Suggested fix (if available)
   - Your contact information and PGP key (if available)

3. **Response Timeline:**
   - **Initial acknowledgment:** Within 48 hours
   - **Assessment:** Within 5 business days
   - **Patch development & testing:** 2-4 weeks (depending on severity)
   - **Public disclosure:** Coordinated with reporter (typically 30-90 days after patch release)

---

## Vulnerability Management

### Severity Levels

- **Critical:** RCE, auth bypass, data exfiltration affecting production systems
- **High:** Local privilege escalation, significant data exposure, DoS
- **Medium:** Information disclosure, minor privilege escalation
- **Low:** Non-security issues, hardening recommendations

### Patching Process

1. **Assessment:** Security team evaluates the report
2. **Reproduction:** Confirm the vulnerability in current and recent versions
3. **Fix:** Develop and test the patch
4. **Release:** Push patch to `main` and release new version (e.g., `v0.2.2`)
5. **Disclosure:** Public advisory with CVE (if applicable)

### Supported Versions

| Version | Supported  | End of Support |
| ------- | ---------- | -------------- |
| 0.2.x   | ✅ Yes     | N/A (current)  |
| 0.1.x   | ⚠️ Limited | 2025-12-31     |
| < 0.1   | ❌ No      | Unsupported    |

**Security patches are provided for the current version and the previous minor version only.**

---

## Security Best Practices for Users

### For Production Deployment

1. **Keep Nacelle Updated**

   ```bash
   # Check for updates
   cargo install --path ./ato-cli --force
   ```

2. **Key Management**
   - Never commit private keys to version control
   - Use environment variables or secure vaults for key storage
   - Regenerate keys if compromised: `ato keygen --name <name>`

3. **Capsule Signing**
   - Always sign capsules with a private key
   - Verify signature before deployment: `ato pack --bundle --manifest ./capsule.toml --key <key>`

4. **Network Security**
   - Use HTTPS/TLS for all remote capsule downloads
   - Validate checksums after download
   - Use firewalls to restrict egress from running capsules

5. **Container Runtime**
   - Run Nacelle engine with minimal privileges (non-root when possible)
   - Use security modules (AppArmor, SELinux) in production
   - Enable audit logging for all deployments

### For Development

1. **Code Review**
   - All changes undergo security review before merge
   - Use `cargo clippy` for static analysis
   - Run `cargo audit` to check for known vulnerabilities

   ```bash
   cargo audit --deny warnings
   ```

2. **Dependencies**
   - Keep all dependencies up-to-date
   - Review dependency changes in PR reviews
   - Use `Cargo.lock` for reproducible builds

3. **Testing**
   - Write security tests for auth, validation, crypto
   - Use fuzzing for parsing/serialization code
   - Perform manual security review for new features

---

## Security Features

### Implemented

- ✅ **Ed25519 Cryptography:** Signature and verification
- ✅ **Capsule Manifest Signing:** Tamper detection
- ✅ **gRPC/TLS Support:** Encrypted communication
- ✅ **Input Validation:** Manifest parsing and validation
- ✅ **OS-Native Sandboxing:** Seatbelt (macOS) / Bubblewrap + Landlock (Linux)
- ✅ **Sensitive Path Protection:** `~/.ssh`, `~/.aws`, `~/.gnupg` etc. auto-denied
- ✅ **Network Isolation:** Per-capsule network access control
- ✅ **Audit Logging:** Deployment and execution logging

### In Development / Planned

- 🔄 **Domain-level Egress Filtering:** Via Sidecar Proxy (tsnet/SOCKS5)
- 🔄 **Encrypted Secrets Management:** Secure secret handling for capsules
- 🔄 **Rate Limiting:** Engine API protection
- 🔄 **Certificate Pinning:** Secure gRPC communication
- 🔄 **Hardware Security Module (HSM) Support:** Key management

---

## Runtime Security Model

### Command Execution Policy: "Allow Any Command"

nacelle does **not** maintain a binary allowlist for capsule commands.
Any command specified in `capsule.toml` (`execution.entrypoint` /
`execution.command`) is permitted to execute.

**Rationale:** Allowing `npm` or `python` already means arbitrary code
can run — `npm run dev` executes whatever is in `package.json`, and
`python script.py` can do anything Python can do. A binary allowlist
creates a false sense of security and breaks extensibility. The real
security boundary is the OS sandbox.

Only basic portability checks are enforced:

- No absolute paths (`/usr/bin/python` → use `python`)
- No directory traversal (`../../../bin/sh`)

### Security Boundary: OS-level Sandbox

The **sole security enforcement** is the OS-native process sandbox:

| Platform | Primary            | Supplementary |
| -------- | ------------------ | ------------- |
| macOS    | Seatbelt (SBPL)    | —             |
| Linux    | Bubblewrap (bwrap) | Landlock LSM  |

#### macOS (Seatbelt / sandbox-exec)

**Strategy: "Allow Default, Deny Sensitive"**

- `(allow default)` — permits general file/process/IPC operations.
- `(deny file-read* file-write* (subpath "~/.ssh") ...)` — blocks
  access to sensitive user directories.
- `(deny network*)` — applied when `isolation.network.enabled = false`.

#### Linux (Bubblewrap + Landlock)

**Strategy: "Namespace Isolation + Filesystem Allow-list"**

1. **Bubblewrap** provides PID/mount/net namespace isolation:
   - Only explicitly bind-mounted paths are visible.
   - Sensitive paths are hidden with `--tmpfs` overlays.
   - `--unshare-all` without `--share-net` blocks network.
2. **Landlock LSM** (kernel 5.13+) adds filesystem access control:
   - Allow-list constructed from `capsule.toml` after filtering
     sensitive paths.
   - Applied as a supplementary layer inside the namespace.

### Sensitive Paths (Default Deny List)

The following user directories are blocked by default across all platforms.
Defined in `src/system/sandbox/mod.rs::sensitive_paths()`.

| Path                                              | Reason              |
| ------------------------------------------------- | ------------------- |
| `~/.ssh`                                          | SSH keys            |
| `~/.gnupg`                                        | GPG keys            |
| `~/.aws`                                          | AWS credentials     |
| `~/.kube`                                         | Kubernetes config   |
| `~/.config/gcloud`                                | GCP credentials     |
| `~/.azure`                                        | Azure credentials   |
| `~/.docker`                                       | Docker credentials  |
| `~/.npmrc`                                        | npm tokens          |
| `~/.pypirc`                                       | PyPI tokens         |
| `~/.bash_history` / `~/.zsh_history`              | May contain secrets |
| `~/Library/Keychains` _(macOS)_                   | System keychain     |
| `~/Library/Cookies` _(macOS)_                     | Browser cookies     |
| `~/Library/Application Support/Chrome` _(macOS)_  | Browser profile     |
| `~/Library/Application Support/Firefox` _(macOS)_ | Browser profile     |

**Behavior when user specifies a parent path:**

If `capsule.toml` contains `read_write = ["~"]` (home directory),
the sandbox automatically excludes sensitive sub-directories:

- **macOS:** Seatbelt `(deny ...)` rules override the parent `(allow ...)`.
- **Linux:** `filter_sensitive_paths()` removes the home directory from
  the Landlock allow-list; Bubblewrap applies `--tmpfs` overlays.

A `WARN` log is emitted when paths are filtered.

### Network Egress Control

| Mechanism                        | Scope            | Status      |
| -------------------------------- | ---------------- | ----------- |
| `isolation.network.enabled`      | All traffic      | ✅ Enforced |
| `isolation.network.egress_allow` | Domain filtering | ⚠️ Sidecar  |

Domain-level egress filtering (`egress_allow`) **cannot** be enforced
at the OS sandbox level (Seatbelt supports only IP-based rules;
Landlock has no network filtering). When `egress_allow` is configured:

1. A `WARN` log is emitted: _"Domain-level egress filtering is not
   enforceable via Seatbelt/Bubblewrap. Relies on Sidecar Proxy."_
2. The Seatbelt profile includes a comment noting the delegation.
3. Actual enforcement is performed by the **Sidecar Proxy (tsnet/SOCKS5)**.

---

## Dependency Security

### Regular Audits

Nacelle dependencies are audited regularly for known vulnerabilities:

```bash
# Audit locally
cargo audit

# In CI (automated)
# Runs on every PR and push to main
```

### Third-Party Dependencies

Core dependencies include:

- **Cryptography:** `ed25519-dalek`, `sha2`, `base64`
- **Serialization:** `serde_json`, `toml`, `capnp`
- **Async Runtime:** `tokio`, `hyper`
- **gRPC:** `tonic`, `prost`
- **Container:** `oci-spec-rs`, `oci-image`

All are vetted for security and maintained actively.

---

## Security Guidelines for Contributors

### Before Submitting a PR

1. Run security checks:

   ```bash
   cargo fmt --check
   cargo clippy --all-targets -- -D warnings
   cargo audit --deny warnings
   cargo test --lib
   ```

2. Review the CONTRIBUTING.md for code style
3. Add tests for security-sensitive changes
4. Document security implications in commit message

### Do's ✅

- ✅ Use `Result<T, E>` for error handling (no `unwrap()` in prod code)
- ✅ Validate all inputs
- ✅ Use constant-time comparisons for cryptographic material
- ✅ Document unsafe code blocks
- ✅ Use security linters (clippy, `cargo-deny`)

### Don'ts ❌

- ❌ Hardcode secrets, API keys, or private keys
- ❌ Use insecure random number generators
- ❌ Ignore type safety (avoid `unsafe` unless necessary)
- ❌ Disable security warnings
- ❌ Use deprecated cryptographic functions

---

## Acknowledgments

Thank you to the security researchers who responsibly report vulnerabilities and help us improve Nacelle's security posture.

---

## Additional Resources

- [OWASP Top 10](https://owasp.org/www-project-top-ten/)
- [Rust Security Guidelines](https://anssi-fr.github.io/rust-guide/)
- [eBPF Security](https://ebpf.io/what-is-ebpf/#security)
- [Container Security Best Practices](https://cheatsheetseries.owasp.org/cheatsheets/Docker_Security_Cheat_Sheet.html)

---

**Last Updated:** February 2026  
**Maintainers:** Nacelle Security Team
