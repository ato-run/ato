# Post-E1 fixture-label audit (not a fourth efficacy arm)

E1's 60-cell comparison is complete and immutable. Neither B nor C executed the
intended positive alternatives in E04/E06/E07. To avoid claiming those labels
were Runtime-verified when they were only syntax-checked, register this separate
validation **before its execution**:

- Exactly three cells: E04/E06/E07, permutation0, fixed owner-authorized draft q7.
- Same frozen source file digests, original K, two-attempt budget, current pinned
  requester/Runtime/WASM. Use existing fixed acceptance provider, no Jev key.
- **Zero model calls**, no provider/model/fixture tuning or repeat E1 cells.
- Check actual same-K fresh receipt acceptance or preserve any failure.
- Do not add these results to A/B/C numerators, denominators, usage or timing.
  They verify intended fixture labels, not selector efficacy. No retry if invalid.
- Stop if the exclusive audit directory already exists.

All twenty E1 model calls remain final. This audit cannot close the failed
E1 efficacy gate or justify a new model call. It is explicitly post-outcome
validation; no prospective oracle-validation claim is made.
