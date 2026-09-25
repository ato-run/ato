# Formation foundation track — working verification ledger

This is an implementation ledger, not a completion declaration. No deployment,
remote migrations, flags, workflows or manual CI reruns are part of this track.

## 2f / PR #1406

Base `de4e4fc3645abfc5d2f6d0f62f2bfc0a95d9427b`.
Head `ab667b82c3f04e40e66598f8e4a2d0d9ca28b02a`.
Implemented and locally/integration verified; unmerged and undeployed.

The actual Coordinator was isolated Miniflare with D1/R2 and the API code at
`e8b0056b48bdb8c3bf0f578edfa610aa0faf0b43` (tree-identical to #691's reviewed
head). The Runtime and requester were native Linux arm64 Rust executables built
from the 2f source. All rows below were `satisfied`, with one accepted route,
`pass`, and a fresh receipt `fully_satisfied=true`:

| case | attempt | K | D |
|---|---|---|---|
| Static | `01M3BZPV0P4EYHMQ949BAXD17Z` | `sha256:480f2e7a190c1047f19447d80416e3582b4a2943a90b9bb8e6a067736145fec6` | `sha256:b4feb08991385b254dd09600a258f27ee7b09b375037db099d8176a4de515d7c` |
| Static 64MiB source | `01M3BZQK9F1WQN83S4ST1WX8YY` | `sha256:03c2fdd7263872afce19987e9148425bccb47b37b8c63ef09ee44995a0e63f9a` | `sha256:468e096962874c33564b789edc86abbac33252ba2d075b39d72cc1fa0c5427d4` |
| Python | `01M3BZYGSGZQY4MKF7C5CT61KQ` | `sha256:22d8402ee34500e845830856d2739abbc7b09bd980d4d2aa40937f96debf65df` | `sha256:3a4c35b63ea012300473b137c0d877a44d7aa27e845a8a17d32ab33ac2f755a7` |
| Node | `01M3BZYPVTFH17JTEBM96FHKRC` | `sha256:ddfa239c2ec8a77ed8b9cb04475562a4be6e10ff563d883cb7edb550e830437d` | `sha256:1d05c0b7bf67e43e343430007330c20e50b1b53cd4d3d9eb4fb96877f15aafdd` |

Process requests with default `network=denied` first produced typed
`network_denied`, `not_started`, no receipt or route. The successful requests
explicitly authorized dependency resolution; no deployment flag was changed.
Harness setup failures (missing destination directory, Node18 instead of Node22,
x86 binary on arm64, incorrect test-token prefix) preceded these successful runs
and are not code-test passes. Request JSON/runtime logs are kept in the task's
`.tmp/2f-actual` and isolated host acceptance directories.

## 3d in progress

Stack base is exactly the #1406 head above, not a merged main.
Rust descriptor and common-launch replay implementation are in progress.
Linux `retained_replay` initially passed all three Static/Python/Node cases:
source Formation, artifact capture, source/build workspace deletion, fresh attempt
on a different runtime identity, same K/D and fresh PASS receipt. These are local
artifact tests, **not durable Coordinator replay acceptance**. The macOS run
only actually executes Static; process cases skip without containment.

Durable object publication/authorization and typed ticket transport are still
being connected. Stage 4, restart acceptance, final P0 remeasurement and final
documentation cleanup are not complete. No foundation completion claim is made.
