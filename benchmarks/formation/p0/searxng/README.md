# searxng — P0 Formation benchmark route

- Upstream: https://github.com/searxng/searxng
- Pinned commit: `2ed96e6fcfc96ca1045155fc52a12f5f7b070417`
- Upstream documented command: `python -m searx.webapp` (`searx/webapp.py`, `run()`), with the
  documented settings variables `SEARXNG_SECRET`, `SEARXNG_BIND_ADDRESS`, `SEARXNG_PORT`.
- Benchmark K0: source identity, `GET /healthz` → 200, `GET /` → 200.
- Benchmark K1 (Browser Contract): "Open the Preferences page and verify that a list of search
  engines or engine categories is shown. Do not run a search."
- Result (baseline): refused at source freeze — the tree contains a symlink
  (`utils/templates/etc/apache2`). The only one of the 20 P0 routes the current Derivation projection can project.
  A counterfactual copy without that symlink reached Level 3 on x86_64 (typed K PASS); see
  `docs/ops/formation-p0-benchmark-2026-09-23.md`.

Use: `ato form <searxng checkout> --runtime-network --route benchmarks/formation/p0/searxng/capsule.toml …`
