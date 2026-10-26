# Preregistered D-conversion repair pilot

Runtime/CLI `fb6f6bb76d13d0bcef1d64346b623cde0f6b255c`; API JS
`b20bba7ab9daf282f8d5b1e22affe549ad3492a3`; receipt authority built from the
same repair Runtime pin. Native binary hashes and WASM hash are in the two
JSON plans. Linux/aarch64, Rust 1.96.0; available disk was above 20 GiB.
Observed live mains remain a separate fact: ato `e494e937...`, API `2dceb36b...`.
These are unmerged Draft implementations, not deployed main behavior.

Select upstream apps **before new results** from the existing evidence:

| Case | Source/K | Purpose |
|---|---|---|
| WBO (2), zero known D | Exact prior archive/source and K | Configuration inspection and actual process startup |
| SVGOMG (57), zero known D | Exact prior archive/source and K | Source-owned build → existing static serving adapter |
| Copyparty (69), zero known D | Exact prior archive/source and K | Previous inspection repetition/information-loss representative |
| SVGOMG, prior-live-D replay | Same exact source and K; replay original CP-generated D `257d10b9...` | Actual prior failure → live CP repair → actual same-K verification |

The fourth case is a **seeded repair gate**, not zero-known-D, a fixture or an
automatic 100-app measurement. The seeded route is the byte-exact prior live
proposal, hash `fd72d67ca17a862a232924c14ed4349ca05bfa72f077d49c73ad0df9360da6b5`.
It previously failed build with exit 127 (`gulp` missing). Replay must fail
again with actual Runtime evidence before claiming failure→repair acceptance.
Do not fabricate a failed round to make this gate pass.

DeepSeek `deepseek-flash`, thinking disabled, output cap 2,048, input cap 24,576;
DecisionProvider Jev 1.13.0 only at a necessary ambiguous finite choice.
Separately frozen prompt v4. Default/effective rounds **3**; unchanged attempt
cap **4**, deadline **900 seconds**, source context **16 KiB**, same Runtime
profile and frozen network/authority ceiling as the previous arm. No source
rewrite, credential binding invention, toolchain or permission expansion.
Dependency/build phase allowlists remain distinct; runtime egress stays denied.

Maximum **12 CP + 4 DP calls**, **128,984 USD micros** reservation. Remaining
before pilot **722,202 micros**; do not start a full 100-app wave under this
remainder. Both pilot run roots belong to one reservation accounting envelope.
Credentials go only to requester children through the existing sealed RAM
channel; values, headers and reasoning are never saved.

Gate outcomes distinguish direct first-generated-D PASS, observed generated
failure→repair PASS, seeded prior-live-D failure→repair PASS, and specific
failure. PASS requires a fresh receipt bound to exact K/D and attempt. Every
case records calls/tokens/cost/latency, canonical D changes and actual
transmitted text digest/length/truncation metadata. No functional acceptance
or persistence claim follows HTTP K. Successful submission is not Run approval.

Keep all raw result/status/journal/receipt hashes. App terminal failures are not
retried. Only proven pre-app infrastructure failure can be corrected within
the same frozen limits. All historical ledgers remain unchanged. No merge,
deployment, remote migration, #1421 execution or E2 72-cell experiment.
