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
4. **Release:** Push patch to `main` and release new version (e.g., `v0.2.1`)
5. **Disclosure:** Public advisory with CVE (if applicable)

### Supported Versions

| Version | Supported | End of Support |
|---------|-----------|----------------|
| 0.2.x   | ✅ Yes    | N/A (current)  |
| 0.1.x   | ⚠️ Limited | 2025-12-31     |
| < 0.1   | ❌ No     | Unsupported    |

**Security patches are provided for the current version and the previous minor version only.**

---

## Security Best Practices for Users

### For Production Deployment

1. **Keep Nacelle Updated**
   ```bash
   # Check for updates
   cargo install --path ./capsule-cli --force
   ```

2. **Key Management**
   - Never commit private keys to version control
   - Use environment variables or secure vaults for key storage
   - Regenerate keys if compromised: `capsule keygen --name <name>`

3. **Capsule Signing**
   - Always sign capsules with a private key
   - Verify signature before deployment: `capsule pack --bundle --manifest ./capsule.toml --key <key>`

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
- ✅ **Sandboxing:** Container isolation (eBPF-based on Linux)
- ✅ **Audit Logging:** Deployment and execution logging

### In Development / Planned

- 🔄 **SELinux/AppArmor Integration:** Enhanced kernel-level isolation
- 🔄 **Encrypted Secrets Management:** Secure secret handling for capsules
- 🔄 **Rate Limiting:** Engine API protection
- 🔄 **Certificate Pinning:** Secure gRPC communication
- 🔄 **Hardware Security Module (HSM) Support:** Key management

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

**Last Updated:** January 2026  
**Maintainers:** Nacelle Security Team
