# OSS quick-start acceptance

Two pieces:

1. **Sample capsules** (one `todo`, one `calendar`, one `excalidraw`), packed
   from their sample sources with the same CLI. Each source must be an
   `ato.capsule/2` directory:
   - TODO and Calendar come from `ato-run/ato-samples` (`samples/shared-json-*`),
     with the operation-scoped `ato.data.toml` / `ato.operations.yaml`.
   - Excalidraw needs a build step: run `npm ci && npm run build` in the sample,
     copy `dist/`, `ato.data.toml`, `ato.operations.yaml` and
     `src/operations/scene.js` into one directory, and add a `capsule.toml`
     whose `[[input]] path = "."` and `[[derivation]] entry = "index.html"`.
   Pack each source:

   ```sh
   ato pack <source-dir> -o capsules/todo.capsule
   ato pack <source-dir> -o capsules/calendar.capsule
   ato pack <source-dir> -o capsules/excalidraw.capsule
   ```

2. **`acceptance.sh`** opens the three capsules as one workspace through the
   in-process Coordinator and checks the lifecycle:

   ```sh
   scripts/oss-quick-start/acceptance.sh \
     --ato path/to/ato --capsules path/to/capsules --port 18846
   # add --install-cmd "<per-host install command>" to include installation
   ```

   It asserts the three panes, the per-instance bindings, the TODO→Calendar
   projection, Excalidraw's scene operation, the checkpoint, and that a resume
   restores the data and the saved split. It prints per-phase times and exits
   non-zero on the first failed assertion.

Measured on macOS arm64 and Linux x86_64 (see
`docs/ops/oss-quick-start-local-e2e-20261110.md`): open/resume are a few
seconds each. The 90-second budget still includes the one-time CLI install and
sample acquisition, which the script reports separately.
