# E1 protocol amendment — comparator integration error

Original registration `410285f3` is immutable. This amendment is committed and
pushed **before repaired B or any further C invocation**. It does not silently
reinterpret the original protocol, whose no-resume rule is explicitly amended.

## Observed failure, not an efficacy result

The B-only validator call used model `evaluation-only`, but the shared production
request validator requires a `jev-` model name. Consequently B returned `invalid`
before scoring. The original run was stopped; 13 cells completed and E03p0B was
interrupted. Four C calls already completed (E01: two PASS; E02: two decline).
These are all retained, including both declines. The original B observations
are invalid-comparator data, not evidence that C is better. The failure was a
harness integration bug missed by pure-selector unit tests.

The repair passes the existing DEFAULT_GENERATION_MODEL to the **local request
validator**; it does not invoke Jev. A new integrated provider test proves that
a valid context reaches a typed draft. Score, tie-break, fixtures, IDs, K,
provider prompt/version/model, runtime and receiver remain unchanged.

## Prospective continuation rules

- Preserve the original experiment directory and all reservations/report files.
- Run into a new exclusive `efficacy-e1-repaired` directory, once only.
- Reuse exactly the nine completed original A/C cells listed in the amendment.
  Require their result-file digest and non-B reservations to match. Ambiguous
  old A/C calls block continuation rather than allowing retry.
- Replace only broken B cells with the repaired evaluation-only comparator.
  Original completed B records are separately committed; interrupted B has no
  final result and is explicitly missing in the original ledger.
- Complete unvisited A/B/C cells. At most **16 additional** C calls; original4
  plus remaining16 never exceeds the preregistered20. Never repeat G9 or any
  previously invoked C cell, whether success or decline.
- Report amended results as such, not as an uninterrupted pristine experiment.
  Counterbalancing was disturbed by the repair; elapsed-time superiority is not
  inferable. No success claim can use the broken B comparator.

No efficacy threshold, fixture, model prompt or deterministic selection rule was
changed after outcomes. Four example tests and Clippy pass after the repair.
Source/binary digests and exact reuse set are in the accompanying JSON.
