# Manual Testing Scripts — ato-cli

> Bash scripts for manual testing of `ato run`, `ato secrets`, `ato encap` commands.

## Quick Start

```bash
cd tests/manual
./test-all.sh           # Run ALL groups
./01-group1-env/test-group1-env.sh    # Run only Group 1
```

## Prerequisites

- `ato` built and installed from source (`cargo install --path .`)
- Internet access (for GitHub repo clones)
- `jq` installed

## Folder Structure

```
tests/manual/
├── config.sh                    # Shared config and helpers
├── README.md                    # This file
├── test-all.sh                  # Run all test suites
├── results/                     # Test outputs (.gitignore'd)
├── 01-group1-env/               # Group 1: Config/Env handling
├── 02-group2-pm/                # Group 2: Package Manager detection
├── 03-group3-secrets/           # Group 3: SecretStore lifecycle
├── 04-group4-targets/           # Group 4: Target type combinations
└── 05-group5-edge/              # Group 5: Edge cases & security
```

## Available Test Suites

| Suite | Group | Cases | Run Command |
|-------|-------|-------|-------------|
| group1-env | Group 1: Config/Env | 1a,1b,1c,1d,1e | `./01-group1-env/test-group1-env.sh` |
| group2-pm | Group 2: Package Manager | 2a,2b,2c,2d,2e | `./02-group2-pm/test-group2-pm.sh` |
| group3-secrets | Group 3: SecretStore | 3a,3b,3c,3d,3e,3f | `./03-group3-secrets/test-group3-secrets.sh` |
| group4-targets | Group 4: Targets | 4a,4b,4c | `./04-group4-targets/test-group4-targets.sh` |
| group5-edge | Group 5: Edge/Security | 5a,5b,5c,5d,5e | `./05-group5-edge/test-group5-edge.sh` |
