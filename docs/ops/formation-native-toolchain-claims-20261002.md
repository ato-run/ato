# Isolated Python native toolchain claims, 2026-10-02

The builtin Python operation isolation change at `5143a6dc` added `-I` to
registered exec argv. `native_runtime_requirements` still recognized only the
old five-argument spelling. Current six-argument Python steps therefore lost
their gcc/make/pkg-config scheduling requirements, even though the contained
operation itself refused missing or mismatched tools.

Recognize the exact isolated Python spelling and preserve the historical Python
and Node spellings. Requirements continue to say `present`; tools are never
provisions. This does not change generated D argv, K, source, network permission,
budgets or deadlines. Both requester and API compiler authority must receive the
same fix before a new native acceptance pin is used.

The focused integration test initially failed on its stale mode-index assertion
before reaching its compiler-claim assertion. It now locates the build mode,
asserts its denied network and the actual gcc requirement, and verifies all seven
registered isolated operations produce the same claims as their historical
spelling. Existing Node native claim coverage remains unchanged. Local validation:
119 Formation unit tests and all 33 exploration integration tests PASS.

Earlier reports of 119 Formation tests refer to library unit coverage. They do
not establish that this integration suite passed after the isolation change.
Earlier real Runtime sdists used a host with bound native tools and consequently
did not expose this scheduling defect. They remain observations at their original
pins; they are not reclassified as final-code acceptance.

Linux independently passed the same 119 unit and 33 integration tests at
execution source `70e8ba07d0aa5a1f16c525d41108db9916f359c6`.

The API regression first reached an admitted proposal with the previous
`5143a6dc` WASM but lacked its gcc requirement. Invalid preliminary fixture
requests are separately retained; they are not that regression. The corrected
request passes with the WASM built from `70e8ba07`, SHA-256
`86cc2c24619488e26dfa3f246538f5e34f6f8589db92528aa8447c9ec4e5fd7b`,
2,123,769 bytes, toolchain 1.96.0. The updated API provenance records this exact
source. Full receipt/Search regression and typecheck results follow in the API
PR. Final native app publication remains pending: successful Runtime K receipts
do not establish a usable retained submission. No deployment or remote migration.
