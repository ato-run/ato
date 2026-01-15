# Contributing to Nacelle

Thank you for your interest in contributing to Nacelle! We welcome contributions from everyone.

## Code of Conduct

This project adheres to the Rust Code of Conduct. By participating, you are expected to uphold this code.

## Getting Started

### Prerequisites

- Rust 1.82+ (installed via [rustup](https://rustup.rs/))
- Git
- For eBPF development: `llvm-14`, `clang-14`, `linux-headers` (Linux only)
- For proto development: `protoc` (Protocol Buffer compiler)

### Development Setup

1. **Clone the repository:**
   ```bash
   git clone https://github.com/nacelle-dev/nacelle.git
   cd nacelle
   ```

2. **Build and test locally:**
   ```bash
   # Build
   cargo build

   # Run tests
   cargo test --lib

   # Check formatting
   cargo fmt --check

   # Lint with Clippy
   cargo clippy --all-targets -- -D warnings

   # Security audit
   cargo audit --deny warnings
   ```

3. **Install pre-commit hooks (recommended):**
   ```bash
   ./scripts/setup-hooks.sh
   ```

---

## Development Workflow

### Branch Strategy

- **`main`** — Production-ready code (protected branch)
- **`feat/***` — Feature branches (e.g., `feat/socket-activation`)
- **`fix/***` — Bug fix branches (e.g., `fix/proto-parsing`)
- **`docs/***` — Documentation branches
- **`chore/***` — Maintenance branches

### Creating a Pull Request

1. **Create a feature branch:**
   ```bash
   git checkout -b feat/your-feature-name
   ```

2. **Make your changes:**
   - Write clean, idiomatic Rust code
   - Add tests for new functionality
   - Update documentation (READMEs, docs/, code comments)
   - Follow the code style guide (see below)

3. **Commit with clear messages:**
   ```bash
   git commit -m "feat: add socket activation support

   - Implement FD passing mechanism
   - Add socket activation tests
   - Update documentation"
   ```

4. **Push and create a PR:**
   ```bash
   git push origin feat/your-feature-name
   ```

5. **Link related issues:**
   - Reference issues with `Closes #123` or `Fixes #456`

### PR Review Process

- **Automated checks:**
  - Formatting: `cargo fmt`
  - Linting: `cargo clippy`
  - Tests: `cargo test`
  - Security: `cargo audit`

- **Code review:**
  - At least one maintainer approval required
  - All conversations must be resolved
  - CI checks must pass

- **Merge:**
  - Squash commits if needed for clarity
  - Rebase onto `main` (no merge commits)

---

## Code Style Guide

### Rust Formatting

- **Use `rustfmt` automatically:**
  ```bash
  cargo fmt
  ```

- **Naming conventions:**
  - Functions/variables: `snake_case`
  - Types/traits: `PascalCase`
  - Constants: `SCREAMING_SNAKE_CASE`
  - Modules: `snake_case`

- **Line length:** 100 characters (soft limit)

### Error Handling

- **Use `Result<T, E>` for fallible operations:**
  ```rust
  // ✅ Good
  fn load_config(path: &Path) -> Result<Config> {
      let data = fs::read_to_string(path)?;
      Ok(serde_json::from_str(&data)?)
  }

  // ❌ Bad (panics on error)
  fn load_config(path: &Path) -> Config {
      let data = fs::read_to_string(path).unwrap();
      serde_json::from_str(&data).unwrap()
  }
  ```

- **Use descriptive error messages:**
  ```rust
  bail!("failed to parse capsule manifest: {}", filename)
  ```

### Documentation

- **Add doc comments to public items:**
  ```rust
  /// Loads a capsule manifest from a TOML file.
  ///
  /// # Arguments
  ///
  /// * `path` - Path to the capsule.toml file
  ///
  /// # Returns
  ///
  /// Returns `Ok(Manifest)` on success or `Err` if parsing fails.
  pub fn load_manifest(path: &Path) -> Result<Manifest> {
      // implementation
  }
  ```

- **Link related items:**
  ```rust
  /// See [`DeployRequest`] for request structure.
  pub struct DeployResponse { }
  ```

### Testing

- **Write tests for public functions:**
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;

      #[test]
      fn test_load_manifest() {
          let manifest = load_manifest("test_data/capsule.toml")
              .expect("failed to load manifest");
          assert_eq!(manifest.name, "test-app");
      }
  }
  ```

- **Use descriptive test names:**
  - ✅ `test_deploy_request_validates_capsule_id()`
  - ❌ `test_deploy()`

### Security-Sensitive Code

- **Mark unsafe blocks:**
  ```rust
  /// # Safety
  ///
  /// This function dereferences a raw pointer. Ensure the pointer is
  /// valid and aligned before calling.
  unsafe fn raw_read(ptr: *const u8) -> u8 {
      *ptr
  }
  ```

- **Use constant-time comparisons for cryptographic material:**
  ```rust
  use subtle::ConstantTimeEq;
  
  let eq = signature.ct_eq(&expected_sig).into();
  ```

- **Never hardcode secrets:**
  - ❌ `const API_KEY: &str = "secret-key-12345"`
  - ✅ Load from environment or secure vault

---

## Commit Message Convention

Follow the [Conventional Commits](https://www.conventionalcommits.org/) format:

```
<type>(<scope>): <subject>

<body>

<footer>
```

### Types

- **feat:** New feature
- **fix:** Bug fix
- **docs:** Documentation changes
- **style:** Code style (formatting, missing semicolons, etc.)
- **refactor:** Code refactoring without behavior change
- **perf:** Performance improvement
- **test:** Adding or updating tests
- **ci:** CI/CD configuration changes
- **chore:** Maintenance tasks (dependencies, build, etc.)
- **security:** Security-related changes

### Examples

```bash
# Feature
git commit -m "feat(engine): add socket activation support"

# Bug fix
git commit -m "fix(cli): handle missing capsule.toml gracefully"

# Documentation
git commit -m "docs: update BUILD.md with eBPF instructions"

# Chore
git commit -m "chore(deps): update tokio to 1.48"
```

---

## Handling TODOs and FIXMEs

- **TODO:** Feature to implement later
- **FIXME:** Known issue or bug to be fixed

### Guidelines

1. **Create an issue first:**
   ```
   Title: Implement feature X
   Description: This feature is needed for ...
   ```

2. **Reference the issue in code:**
   ```rust
   // TODO: Implement graceful shutdown (#123)
   fn shutdown() {
       // current implementation
   }
   ```

3. **Resolve or create tracking issue:**
   - Do not commit code with untracked TODOs to `main`
   - For new experimental features, create a feature flag

---

## Testing Guidelines

### Unit Tests

```bash
cargo test --lib
```

### Integration Tests

```bash
cargo test --test '*'
```

### Smoke Tests (Samples)

```bash
./scripts/test-samples.sh
```

### Full Test Suite

```bash
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo audit --deny warnings
```

---

## Documentation Standards

### README Updates

- Update `README.md` if you change:
  - Installation instructions
  - Usage examples
  - Feature descriptions
  - Project structure

### Build Documentation

- Update `docs/BUILD.md` if you change:
  - Build dependencies
  - Build process
  - Supported platforms

### Sample Applications

- Add a new sample to `samples/` with:
  - README.md (description, prerequisites, quick start)
  - capsule.toml (valid manifest)
  - .gitignore (language-specific)
  - Build script (build.sh or Makefile)

---

## Security Considerations

### Before Submitting

1. **Run security audit:**
   ```bash
   cargo audit --deny warnings
   ```

2. **Check for secrets:**
   ```bash
   # Search for hardcoded credentials
   git diff HEAD~1 | grep -E "secret|key|password|token"
   ```

3. **Review unsafe code:**
   - Minimize `unsafe` blocks
   - Document why `unsafe` is necessary
   - Ensure memory safety

4. **Validate inputs:**
   - Always validate user-provided data
   - Use type-safe abstractions
   - Avoid string parsing for security-critical data

---

## Reporting Issues

### Bug Reports

Include:

- **Description:** What is the issue?
- **Steps to reproduce:** How do you trigger the bug?
- **Expected behavior:** What should happen?
- **Actual behavior:** What actually happens?
- **Environment:** OS, Rust version, Nacelle version
- **Logs/Output:** Error messages or stack traces

### Feature Requests

Include:

- **Use case:** Why is this feature needed?
- **Proposed solution:** How should it work?
- **Alternatives considered:** What else could work?

---

## Getting Help

- **Documentation:** [docs/](docs/) and [SECURITY.md](SECURITY.md)
- **GitHub Issues:** [Bug reports and feature requests](https://github.com/nacelle-dev/nacelle/issues)
- **Discussions:** [Community questions](https://github.com/nacelle-dev/nacelle/discussions)
- **Email:** [Contact the team](mailto:hello@nacelle.dev)

---

## Recognition

All contributors will be:

- Added to the `CONTRIBUTORS` file (if applicable)
- Credited in release notes for significant contributions
- Thanked in PR comments and discussions

Thank you for making Nacelle better! 🎉
