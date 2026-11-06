# Licensing

Status: Current reference.

This page indexes the license terms that apply to material in this repository.
It does not change a license, and it does not grant rights beyond the terms it
points to.

| Material | Terms | Location |
|---|---|---|
| Project-authored code | Mozilla Public License 2.0 (`MPL-2.0`) | [LICENSE](../LICENSE) |
| CLI package and binary | MPL-2.0; the Apache-2.0 text is retained for the WiX template and previously distributed builds | [apps/cli/LICENSE](../apps/cli/LICENSE), [apps/cli/LICENSE-APACHE](../apps/cli/LICENSE-APACHE) |
| WiX installer template | Upstream Apache-2.0 text retained | [apps/cli/wix/main.wxs](../apps/cli/wix/main.wxs), [apps/cli/LICENSE-APACHE](../apps/cli/LICENSE-APACHE) |
| CPython fixture | The upstream Python license retained | [apps/formation-worker/fixtures/proposal-http/LICENSE.cpython](../apps/formation-worker/fixtures/proposal-http/LICENSE.cpython) |
| AODD skill | MIT | [.claude/skills/aodd/LICENSE](../.claude/skills/aodd/LICENSE) |
| Dependencies | Their own licenses, as recorded by their manifests and lockfiles | the dependency trees and lockfiles |

## Notes

- Third-party code, dependencies, patches containing upstream code, and
  materials with their own license notices retain their respective terms.
- Previously granted licenses remain in effect.
- The code license does not establish a new license for the Capsule Process
  Model's theoretical documents or the RFCs under `docs/theory/` and
  `docs/rfcs/`; their original terms apply.
- To identify the terms for a specific file, check its header and any adjacent
  license or notice file. If no notice is present and the ownership is unclear,
  open an issue before assuming a license.
