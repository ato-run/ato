# Component Contract observation implementation — 2026-10-24

Dependent on the explicit service-dependency authoring change (ato#1504).
The independent Linux source-OCI compiler fix ato#1505 is required when building
the CLI on Linux. Neither change alters an existing K or D.

## Local verification

- Targeted Cargo check of portable application, CLI and connected worker: PASS.
- The same three crates with `--all-targets`: PASS.
- Portable application library: 105 tests PASS, including byte-identity fixtures,
  static/process/OCI routes, independent imports and state/snapshot regression.
- Added component Contract test: missing component, wrong status and wrong Port
  all fail the whole K; complete evidence passes; unknown Port is refused.
- Added real-socket Runner test: exact signed Port/path is required, wrong Port
  and path/injection are refused, and no proof reaches the private upstream.

The local Mac was heavily swapping (16GiB RAM); elapsed build/test time is not
runtime performance evidence. API D1 observer tests and Linux OCI tests are
recorded below once complete. No production deploy, paid GPU or operator key
use is involved in these tests.
