# OpenVal browser preview

This adapter runs the unmodified OpenVal cashflow engine from upstream commit
`89f471751e498813c8458cdc1f6bdc5a54b78e2a` (MIT) in a pinned, locally served
Pyodide 0.29.4 worker. It changes the upstream workbook reader only to interpret
a blank optional gross-up field as absent, so a default workbook exported by
the app can be imported again. It is pre-alpha software, not validated for
actual underwriting. Property data is stored in the user's browser and the
adapter sends no property data to a server.

The frontend uses Next.js 16.4.5 and React 18.3.1, with the exact package lock
and Python wheels in this directory. `source_archive.py` includes the upstream
source, changed files, adapter, and notices. Pyodide and CPython are not
modified; the archive identifies their exact upstream source versions.

To reproduce, run `python3 assemble.py <new-empty-directory>` from an
environment with Python 3, Node and npm. It verifies pinned archive, wheel and
license digests, builds the static export, and prints its path. This step
downloads public dependencies; the resulting site uses its own origin at
runtime. Copy `capsule.template.toml` into that export as `capsule.toml`, then
create a portable static bundle with the `ato.portable-tree/2` schema (the
older tree profile rejects some binary files). Do not commit generated
dependency trees or output artifacts.

The build uses webpack and moves Next's inline bootstrap scripts into local
files. This accommodates the Instance gateway's versioned asset URLs and
allows delivery under a CSP that forbids inline scripts.

On 2026-11-05 the original and a cleanly rebuilt browser export completed a
real Chrome test with all non-local network requests blocked: annual and
monthly reports matched the upstream Python engine within numeric tolerance;
the default workbook was exported and reimported; the upstream sample workbook
was uploaded; a workbook was downloaded; and the page reloaded successfully.
The clean `npm ci` audit reported zero known vulnerabilities. These checks
do not establish correctness for all inputs or production financial use.
