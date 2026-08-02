# Static Web Manifest JCS v1 fixture source

- Source repository: `ato-run/ato-contents`
- Source merge commit: `ce10d7350e2bd75173d5b3f12ad52331aa71eaab` (ato-contents PR #3)
- Source directory: `docs/contracts/fixtures/static-web-manifest-jcs-v1/`
- Schema: `ato.static-web-manifest/v1`
- Source file list: `input.json`, `canonical.json`, `expected.json`; the fixed
  frame-ancestor corpus is copied from
  `docs/contracts/fixtures/static-web-frame-ancestors-v1/{valid,invalid}.json`.

The source fixture is copied byte-for-byte from merged `main` to make Rust
producer/Worker canonicalization divergence a test failure.
