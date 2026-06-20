# Capsule Protocol Conformance Suite

This directory contains the conformance test suite for the [Capsule Protocol spec](../docs/current-spec.md).

A **conformant ato implementation** must pass all tests marked `REQUIRED` in this suite.
Tests marked `OPTIONAL` cover extension points that implementations may omit.

## Status

This suite is a skeleton as of v0.5. Tests will be populated incrementally alongside
spec stabilization.

**Current coverage: 0 / 0 tests (suite not yet populated)**

## Purpose

The conformance suite exists to:

1. Enable **external runtime implementations** to verify compatibility with the
   Capsule Protocol without requiring access to the reference ato implementation.
2. Provide a **machine-readable gate** for the Foundation transfer KPI
   (≥70% conformance suite pass rate required — see §11.2 of the spec).
3. Give capsule authors a way to verify that their `capsule.toml` manifests will
   behave consistently across conformant implementations.

## Running

```bash
# Run all conformance tests against the local ato binary
./run.sh --binary $(which ato)

# Run against a specific binary
./run.sh --binary /path/to/other-ato-impl
```

> **Note:** The test runner script (`run.sh`) is not yet implemented. This file
> documents the intended interface.

## Structure (planned)

```
conformance/
├── README.md                  # This file
├── run.sh                     # Test runner (not yet implemented)
├── manifest/                  # capsule.toml parsing and validation tests
│   ├── v0.3-required-fields/
│   ├── schema-validation/
│   └── unknown-fields-tolerance/
├── runtime/                   # Runtime execution contract tests
│   ├── source-python/
│   ├── source-node/
│   └── wasm/
├── isolation/                 # Sandbox boundary tests
│   ├── network-deny-default/
│   ├── egress-allow/
│   └── filesystem-workspace/
└── ipc/                       # IPC protocol tests
    ├── handshake/
    └── capability-gating/
```

## Contributing

External runtime implementors are welcome to add test cases. Open a PR with:

- A directory under the appropriate category
- A `test.sh` script that exits 0 on pass, non-zero on fail
- A `README.md` describing what the test verifies and which spec section it covers
- The REQUIRED or OPTIONAL tag in the README

## External implementations

If you have built a conformant ato runtime, please open a PR to add your
implementation to `IMPLEMENTATIONS.md` (not yet created).
